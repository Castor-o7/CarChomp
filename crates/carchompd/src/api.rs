//! HTTP surface: one WebSocket for everything live, a little REST for
//! everything stored. PostgreSQL builds the JSON, so there are no row structs
//! to keep in step with the schema.

use crate::{Bus, Config, Status, recorder, system};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        ConnectInfo, Path, Query, Request, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use carchomp_core::{
    Fix, interchange,
    beacon::{Params, SmartBeacon},
};
use serde::Deserialize;
use sqlx::PgPool;
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
};
use time::{Duration, OffsetDateTime};
use tokio::sync::{Semaphore, broadcast::error::RecvError, watch};
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
struct App {
    db: PgPool,
    bus: Bus,
    status: watch::Sender<Status>,
    /// Where the bulk data lives; its disk is the one worth watching.
    data_dir: PathBuf,
    /// One import at a time: each can hold a large body and many rows.
    importing: Arc<Semaphore>,
    /// WebSocket clients at once; each holds a socket and a task.
    clients: Arc<Semaphore>,
    /// Imports are thinned the way live fixes are.
    beacon: Params,
}

const IMPORT_LIMIT: usize = 16 << 20;
const IMPORT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const MAX_CLIENTS: usize = 32;
/// Clients only ever close or answer pings; nothing they send is read.
const WS_MESSAGE_LIMIT: usize = 4096;
/// `near` radius cap, metres: enough for any "nearby", small enough for the index.
const MAX_NEAR: f64 = 50_000.0;
/// `minutes` cap: about ten years.
const MAX_MINUTES: f64 = 5_256_000.0;

pub fn router(db: PgPool, bus: Bus, status: watch::Sender<Status>, config: &Config) -> Router {
    let router = Router::new()
        .route("/ws", get(ws))
        .route("/api/health", get(health))
        .route("/api/tracks", get(list_tracks))
        .route("/api/tracks/import", post(import_tracks))
        .route("/api/tracks/export.gpx", get(export_tracks))
        .route("/api/tracks/{id}", get(get_track).patch(patch_track).delete(delete_track))
        .route("/api/tracks/{id}/gpx", get(get_track_gpx))
        .route("/api/segments", get(segments))
        .route("/api/aprs/stations", get(aprs_stations))
        .route("/api/aprs/bulletins", get(aprs_bulletins))
        // Without this, a mistyped API path would be answered with the UI.
        .route("/api/{*unknown}", any(|| async { StatusCode::NOT_FOUND }))
        .route_layer(middleware::from_fn(same_origin))
        .with_state(App {
            db,
            bus,
            status,
            data_dir: config.maps_dir.clone(),
            importing: Arc::new(Semaphore::new(1)),
            clients: Arc::new(Semaphore::new(MAX_CLIENTS)),
            beacon: config.beacon,
        });
    match config.ui_dir.clone() {
        Some(dir) => {
            let index = ServeFile::new(dir.join("index.html"));
            router.fallback_service(ServeDir::new(dir).fallback(index))
        }
        None => router,
    }
}

struct Error(sqlx::Error);

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        Self(e)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        match self.0 {
            sqlx::Error::RowNotFound => StatusCode::NOT_FOUND.into_response(),
            // SQLSTATE class 22, data exception: the client's input was bad.
            sqlx::Error::Database(e) if e.code().is_some_and(|c| c.starts_with("22")) => {
                tracing::warn!("api: {e}");
                StatusCode::BAD_REQUEST.into_response()
            }
            e => {
                tracing::error!("api: {e}");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

/// Refuses what a web page elsewhere could make a browser send here: requests
/// under a public host name (DNS rebinding) and requests from another site's
/// page. Browsers apply no CORS to WebSockets or to simple POSTs, so this is
/// the only thing between any page a passenger opens and the car's position.
pub async fn same_origin(req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|v| v.to_str().ok());
    let host = host.or(req.uri().authority().map(|a| a.as_str()));
    let origin = req.headers().get(header::ORIGIN).map(|v| v.to_str().unwrap_or("null"));
    if trusted(host, origin) {
        next.run(req).await
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}

/// `host` must be an address or a name only the local network can resolve;
/// `origin`, when a browser sends one, must be a page on that same host.
fn trusted(host: Option<&str>, origin: Option<&str>) -> bool {
    let Some(host) = host else { return false };
    let name = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    };
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    let local = name.parse::<std::net::IpAddr>().is_ok()
        || (!name.is_empty() && !name.contains('.'))
        || [".local", ".localhost", ".lan", ".home.arpa", ".internal"].iter().any(|s| name.ends_with(s));
    let same = match origin {
        None => true,
        Some(origin) => origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
            .is_some_and(|authority| authority.eq_ignore_ascii_case(host)),
    };
    local && same
}

/// A network in CIDR notation; a bare address is a network of one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Net {
    addr: IpAddr,
    prefix: u8,
}

impl Net {
    fn parse(s: &str) -> Option<Net> {
        let (addr, prefix) = match s.trim().split_once('/') {
            Some((addr, prefix)) => (addr.parse::<IpAddr>().ok()?, Some(prefix.parse::<u8>().ok()?)),
            None => (s.trim().parse::<IpAddr>().ok()?, None),
        };
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = prefix.unwrap_or(max);
        if prefix > max {
            return None;
        }
        // Peers are compared unmapped, so ::ffff:a.b.c.d/n is a.b.c.d/(n - 96).
        match addr.to_canonical() {
            IpAddr::V4(v4) if addr.is_ipv6() => Some(Net { addr: v4.into(), prefix: prefix.checked_sub(96)? }),
            addr => Some(Net { addr, prefix }),
        }
    }

    fn contains(&self, ip: IpAddr) -> bool {
        // Keep the top `prefix` bits of a `bits`-wide address.
        let top = |bits: u32, x: u128| if self.prefix == 0 { 0 } else { x >> (bits - u32::from(self.prefix)) };
        match (self.addr, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => top(32, u32::from(net).into()) == top(32, u32::from(ip).into()),
            (IpAddr::V6(net), IpAddr::V6(ip)) => top(128, net.into()) == top(128, ip.into()),
            _ => false,
        }
    }
}

/// Parse `trusted_networks`, naming the first entry that is not a network.
pub fn networks(list: &[String]) -> Result<Vec<Net>, String> {
    list.iter()
        .map(|s| Net::parse(s).ok_or_else(|| format!("trusted_networks: {s:?} is not an address or CIDR network")))
        .collect()
}

/// Loopback always, otherwise only the listed networks. IPv4 peers on a
/// dual-stack socket arrive as ::ffff:a.b.c.d, which counts as a.b.c.d.
fn peer_allowed(networks: &[Net], peer: IpAddr) -> bool {
    let peer = peer.to_canonical();
    peer.is_loopback() || networks.iter().any(|n| n.contains(peer))
}

/// Serves the car's own hotspot and the device itself, and nobody else:
/// everything here, the car's position included, is unauthenticated.
pub async fn trusted_peer(
    State(networks): State<Arc<Vec<Net>>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if peer_allowed(&networks, peer.ip()) {
        return next.run(req).await;
    }
    let body = "This CarChomp only serves its own hotspot and the device itself.\n\
                To allow another network, add it (for example \"192.168.1.0/24\") to trusted_networks\n\
                in /etc/carchomp/carchompd.toml and restart carchompd.\n";
    (StatusCode::FORBIDDEN, body).into_response()
}

/// Run a query that returns a single JSON text value and pass it through.
async fn json(query: sqlx::query::QueryScalar<'_, sqlx::Postgres, String, sqlx::postgres::PgArguments>, db: &PgPool) -> Result<Response, Error> {
    let body = query.fetch_one(db).await?;
    Ok(([(header::CONTENT_TYPE, "application/json")], body).into_response())
}

async fn ws(State(app): State<App>, upgrade: WebSocketUpgrade) -> Response {
    let Ok(permit) = app.clients.clone().try_acquire_owned() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "too many live clients").into_response();
    };
    upgrade
        .max_message_size(WS_MESSAGE_LIMIT)
        .max_frame_size(WS_MESSAGE_LIMIT)
        .on_upgrade(|socket| async move {
            live(socket, app).await;
            drop(permit);
        })
}

/// Messages are either `{"status": {...}}` or `{"source": n, "obs": {...}}`.
async fn live(mut socket: WebSocket, app: App) {
    let mut events = app.bus.subscribe();
    let mut status = app.status.subscribe();
    status.mark_changed(); // so a new client hears the current status at once
    loop {
        let text = tokio::select! {
            event = events.recv() => match event {
                Ok(event) => serde_json::to_string(&event),
                Err(RecvError::Lagged(_)) => continue, // a slow client just skips ahead
                Err(RecvError::Closed) => return,
            },
            Ok(()) = status.changed() => {
                serde_json::to_string(&serde_json::json!({ "status": *status.borrow_and_update() }))
            }
            // Clients have nothing to say; this only notices them leaving.
            msg = socket.recv() => match msg {
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                _ => return,
            },
        };
        let text = text.expect("messages serialize");
        if socket.send(Message::Text(text.into())).await.is_err() {
            return;
        }
    }
}

async fn health(State(app): State<App>) -> Result<Response, Error> {
    let q = sqlx::query_scalar(
        "SELECT json_build_object(
            'version', $1::text,
            'current_track', (SELECT max(id) FROM track WHERE ended IS NULL),
            'tracks', (SELECT count(*) FROM track),
            -- Polled every few seconds: estimate once exact counting gets slow.
            'fixes', (SELECT CASE WHEN reltuples > 1e5 THEN reltuples::bigint ELSE (SELECT count(*) FROM fix) END
                      FROM pg_class WHERE oid = 'fix'::regclass),
            'aprs_stations', (SELECT count(DISTINCT callsign) FROM aprs_packet WHERE time > now() - interval '1 day'),
            'database_bytes', pg_database_size(current_database()))::text",
    )
    .bind(VERSION);
    let body: String = q.fetch_one(&app.db).await?;
    let mut health: serde_json::Value = serde_json::from_str(&body).expect("PostgreSQL writes valid JSON");
    health["system"] = system::snapshot(&app.data_dir);
    Ok(Json(health).into_response())
}

/// The release this daemon was built from (see tools/bundle.sh), else the crate version.
pub const VERSION: &str = match option_env!("CARCHOMP_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Deserialize)]
struct Near {
    /// `lon,lat,metres`: only tracks passing within that distance.
    near: Option<String>,
}

async fn list_tracks(State(app): State<App>, Query(q): Query<Near>) -> Result<Response, Error> {
    let (lon, lat, r) = match q.near.as_deref().map(floats) {
        Some(Some([lon, lat, r])) if (-180.0..=180.0).contains(&lon) && (-90.0..=90.0).contains(&lat) && r >= 0.0 => {
            (Some(lon), Some(lat), Some(r.min(MAX_NEAR)))
        }
        Some(_) => return Ok(StatusCode::BAD_REQUEST.into_response()),
        None => (None, None, None),
    };
    let q = sqlx::query_scalar(
        "SELECT coalesce(json_agg(t ORDER BY t.started DESC), '[]')::text FROM (
            SELECT id, name, started, ended, visible,
                   (SELECT round(sum(ST_Length(geom))) FROM track_segment s WHERE s.track_id = track.id) AS metres
            FROM track
            WHERE $1::float8 IS NULL OR EXISTS (
                SELECT 1 FROM track_segment s
                WHERE s.track_id = track.id
                  AND ST_DWithin(s.geom, ST_MakePoint($1, $2)::geography, $3))
         ) t",
    )
    .bind(lon)
    .bind(lat)
    .bind(r);
    json(q, &app.db).await
}

/// One track as a GeoJSON Feature.
async fn get_track(State(app): State<App>, Path(id): Path<i64>) -> Result<Response, Error> {
    let q = sqlx::query_scalar(
        "SELECT json_build_object(
            'type', 'Feature',
            'properties', json_build_object('id', id, 'name', name, 'started', started, 'ended', ended),
            'geometry', (SELECT ST_AsGeoJSON(ST_MakeLine(geom ORDER BY time), 6)::json
                         FROM fix WHERE track_id = track.id))::text
         FROM track WHERE id = $1",
    )
    .bind(id);
    json(q, &app.db).await
}

async fn get_track_gpx(State(app): State<App>, Path(id): Path<i64>) -> Result<Response, Error> {
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM track WHERE id = $1").bind(id).fetch_one(&app.db).await?;
    let rows: Vec<(OffsetDateTime, f64, f64, Option<f32>)> =
        sqlx::query_as("SELECT time, ST_X(geom), ST_Y(geom), alt FROM fix WHERE track_id = $1 ORDER BY time")
            .bind(id)
            .fetch_all(&app.db)
            .await?;
    let points: Vec<_> = rows
        .into_iter()
        .map(|(time, lon, lat, ele)| interchange::Point { lon, lat, ele: ele.map(f64::from), time: Some(time) })
        .collect();
    let headers = [
        (header::CONTENT_TYPE, "application/gpx+xml".to_owned()),
        (header::CONTENT_DISPOSITION, format!("attachment; filename=\"track-{id}.gpx\"")),
    ];
    Ok((headers, interchange::to_gpx(name.as_deref(), &points)).into_response())
}

/// Accepts a GPX or GeoJSON document; responds with the new track ids.
/// All of it is imported or none of it is.
async fn import_tracks(State(app): State<App>, body: Body) -> Result<Response, Error> {
    let Ok(_permit) = app.importing.try_acquire() else {
        return Ok((StatusCode::SERVICE_UNAVAILABLE, "another import is running").into_response());
    };
    let body = match tokio::time::timeout(IMPORT_READ_TIMEOUT, axum::body::to_bytes(body, IMPORT_LIMIT)).await {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => return Ok((StatusCode::PAYLOAD_TOO_LARGE, "upload too large (16 MB at most) or interrupted").into_response()),
        Err(_) => return Ok((StatusCode::REQUEST_TIMEOUT, "upload too slow").into_response()),
    };
    let Ok(body) = std::str::from_utf8(&body) else {
        return Ok((StatusCode::BAD_REQUEST, "not UTF-8 text").into_response());
    };
    let tracks = match interchange::parse(body) {
        Ok(tracks) if tracks.is_empty() => return Ok((StatusCode::BAD_REQUEST, "no tracks found").into_response()),
        Ok(tracks) => tracks,
        Err(e) => return Ok((StatusCode::BAD_REQUEST, e).into_response()),
    };
    let source = recorder::source_id(&app.db, "import", "upload").await?;
    let now = OffsetDateTime::now_utc();
    let mut ids = Vec::new();
    let result: Result<(), sqlx::Error> = async {
        for track in tracks {
            // Time is part of a fix's identity. Points that come without one get a
            // second apiece, counted from the moment of import.
            let timed = track.points.iter().all(|p| p.time.is_some());
            let mut fixes: Vec<Fix> = (0..)
                .zip(&track.points)
                .map(|(i, p)| Fix {
                    time: p.time.filter(|_| timed).unwrap_or(now + Duration::seconds(i)),
                    lat: p.lat,
                    lon: p.lon,
                    alt: p.ele,
                    speed: None,
                    course: None,
                    h_err: None,
                })
                .collect();
            distinct_times(&mut fixes);
            // Stored the way a live drive is. Without times there is no
            // speed to beacon by, so an untimed track (a planned route) is kept whole.
            if timed {
                fixes = thin(fixes, app.beacon);
            }
            let id: i64 = sqlx::query_scalar("INSERT INTO track (name, started) VALUES ($1, $2) RETURNING id")
                .bind(&track.name)
                .bind(fixes[0].time)
                .fetch_one(&app.db)
                .await?;
            ids.push(id);
            recorder::insert_fixes(&app.db, source, id, &fixes).await?;
            recorder::finish_track(&app.db, id).await?;
        }
        Ok(())
    }
    .await;
    if let Err(e) = result {
        // Undo what was written; anything this misses is closed at the next start.
        let _ = sqlx::query("DELETE FROM track WHERE id = ANY($1)").bind(&ids).execute(&app.db).await;
        return Err(e.into());
    }
    // finish_track drops tracks with nothing to draw.
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM track WHERE id = ANY($1) ORDER BY id")
        .bind(&ids)
        .fetch_all(&app.db)
        .await?;
    if ids.is_empty() {
        return Ok((StatusCode::BAD_REQUEST, "no track has two distinct points").into_response());
    }
    Ok(Json(ids).into_response())
}

/// Two points of one track may not share a time, which is part of a fix's
/// identity; loggers that sample faster than they stamp repeat whole seconds.
/// Repeats are moved on by a microsecond (the database's resolution) apiece.
fn distinct_times(fixes: &mut [Fix]) {
    let mut taken = HashSet::new();
    let mut next: HashMap<OffsetDateTime, OffsetDateTime> = HashMap::new();
    for fix in fixes {
        let stamped = fix.time.replace_nanosecond(fix.time.nanosecond() / 1000 * 1000).expect("still in range");
        let mut time = next.get(&stamped).copied().unwrap_or(stamped);
        while !taken.insert(time) {
            time += Duration::microseconds(1);
        }
        next.insert(stamped, time + Duration::microseconds(1));
        fix.time = time;
    }
}

/// What smart beaconing would have stored of a recorded track, plus always
/// its last point. Imported points carry no speed or course, so the beacon
/// is fed ones derived from each point and the one before it (the first
/// point: the one after it); the fixes kept are returned as they came.
/// Times must be distinct.
fn thin(fixes: Vec<Fix>, params: Params) -> Vec<Fix> {
    let mut beacon = SmartBeacon::new(params);
    let mut kept = HashSet::new();
    for (i, fix) in fixes.iter().enumerate() {
        let mut fed = fix.clone();
        let pair = if i == 0 { fixes.get(..2) } else { fixes.get(i - 1..=i) };
        if let Some([prev, fix]) = pair {
            let seconds = (fix.time - prev.time).as_seconds_f64();
            fed.speed = fed.speed.or((seconds > 0.0).then(|| distance(prev, fix) / seconds));
            fed.course = fed.course.or(Some(bearing(prev, fix)));
        }
        kept.extend(beacon.push(&fed).into_iter().map(|f| f.time));
    }
    kept.extend(fixes.last().map(|f| f.time));
    fixes.into_iter().filter(|f| kept.contains(&f.time)).collect()
}

/// Great-circle distance in metres.
fn distance(a: &Fix, b: &Fix) -> f64 {
    let (p1, p2) = (a.lat.to_radians(), b.lat.to_radians());
    let h = ((p2 - p1) / 2.0).sin().powi(2) + p1.cos() * p2.cos() * ((b.lon - a.lon).to_radians() / 2.0).sin().powi(2);
    2.0 * 6_371_008.8 * h.sqrt().min(1.0).asin()
}

/// Initial bearing from `a` to `b`, degrees true, 0..360.
fn bearing(a: &Fix, b: &Fix) -> f64 {
    let (p1, p2, dl) = (a.lat.to_radians(), b.lat.to_radians(), (b.lon - a.lon).to_radians());
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    y.atan2(x).to_degrees().rem_euclid(360.0)
}

/// Every visible track, each its own <trk>, in one GPX file.
async fn export_tracks(State(app): State<App>) -> Result<Response, Error> {
    let tracks: Vec<(i64, Option<String>)> =
        sqlx::query_as("SELECT id, name FROM track WHERE visible ORDER BY started").fetch_all(&app.db).await?;
    let mut all = Vec::new();
    for (id, name) in tracks {
        let rows: Vec<(OffsetDateTime, f64, f64, Option<f32>)> =
            sqlx::query_as("SELECT time, ST_X(geom), ST_Y(geom), alt FROM fix WHERE track_id = $1 ORDER BY time")
                .bind(id)
                .fetch_all(&app.db)
                .await?;
        let points = rows
            .into_iter()
            .map(|(time, lon, lat, ele)| interchange::Point { lon, lat, ele: ele.map(f64::from), time: Some(time) })
            .collect();
        all.push((name, points));
    }
    let headers = [
        (header::CONTENT_TYPE, "application/gpx+xml"),
        (header::CONTENT_DISPOSITION, "attachment; filename=\"carchomp-tracks.gpx\""),
    ];
    Ok((headers, gpx_all(&all)).into_response())
}

/// One GPX document holding each track's <trk> as `to_gpx` writes it.
fn gpx_all(tracks: &[(Option<String>, Vec<interchange::Point>)]) -> String {
    let one = interchange::to_gpx(None, &[]);
    let (head, tail) = (&one[..one.find("<trk>").expect("to_gpx writes a trk")], "</gpx>\n");
    let mut gpx = head.to_owned();
    for (name, points) in tracks {
        let doc = interchange::to_gpx(name.as_deref(), points);
        let (start, end) = (doc.find("<trk>").expect("a trk"), doc.rfind("</trk>").expect("a trk") + "</trk>\n".len());
        gpx += &doc[start..end];
    }
    gpx + tail
}

#[derive(Deserialize)]
struct Viewport {
    /// `west,south,east,north`
    bbox: String,
}

/// Geometry of every visible track inside the viewport, one Feature per
/// track. This is what the map draws.
async fn segments(State(app): State<App>, Query(q): Query<Viewport>) -> Result<Response, Error> {
    let Some([w, s, e, n]) = floats(&q.bbox) else {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    };
    let q = sqlx::query_scalar(
        "SELECT json_build_object('type', 'FeatureCollection', 'features', coalesce(json_agg(f), '[]'))::text FROM (
            SELECT json_build_object(
                'type', 'Feature',
                'properties', json_build_object('track', track_id),
                'geometry', ST_AsGeoJSON(ST_Collect(s.geom::geometry), 6)::json) AS f
            FROM track_segment s JOIN track ON track.id = s.track_id
            WHERE track.visible AND s.geom::geometry && ST_MakeEnvelope($1, $2, $3, $4, 4326)
            GROUP BY track_id) t",
    )
    .bind(w)
    .bind(s)
    .bind(e)
    .bind(n);
    json(q, &app.db).await
}

/// Parse exactly `N` comma-separated numbers.
fn floats<const N: usize>(s: &str) -> Option<[f64; N]> {
    let v: Vec<f64> = s.split(',').map(|x| x.trim().parse::<f64>().ok().filter(|v| v.is_finite())).collect::<Option<_>>()?;
    v.try_into().ok()
}

#[derive(Deserialize)]
struct TrackPatch {
    name: Option<String>,
    visible: Option<bool>,
}

async fn patch_track(State(app): State<App>, Path(id): Path<i64>, Json(p): Json<TrackPatch>) -> Result<StatusCode, Error> {
    let done = sqlx::query("UPDATE track SET name = coalesce($2, name), visible = coalesce($3, visible) WHERE id = $1")
        .bind(id)
        .bind(p.name)
        .bind(p.visible)
        .execute(&app.db)
        .await?;
    Ok(found(done.rows_affected()))
}

/// Only finished tracks: the recorder (or an import) is still writing an open one.
async fn delete_track(State(app): State<App>, Path(id): Path<i64>) -> Result<StatusCode, Error> {
    let done = sqlx::query("DELETE FROM track WHERE id = $1 AND ended IS NOT NULL").bind(id).execute(&app.db).await?;
    if done.rows_affected() == 0 {
        let open: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM track WHERE id = $1)").bind(id).fetch_one(&app.db).await?;
        if open {
            return Ok(StatusCode::CONFLICT);
        }
    }
    Ok(found(done.rows_affected()))
}

fn found(rows: u64) -> StatusCode {
    if rows == 0 { StatusCode::NOT_FOUND } else { StatusCode::NO_CONTENT }
}

#[derive(Deserialize)]
struct Since {
    /// Only what was heard in the last this-many minutes.
    minutes: Option<f64>,
}

/// `minutes`, or the default when absent; `None` if it is not a sane span.
fn minutes(given: Option<f64>, default: f64) -> Option<f64> {
    let m = given.unwrap_or(default);
    (m.is_finite() && (0.0..=MAX_MINUTES).contains(&m)).then_some(m)
}

/// Stations and objects on the map, as a GeoJSON FeatureCollection. Default: the last hour.
async fn aprs_stations(State(app): State<App>, Query(q): Query<Since>) -> Result<Response, Error> {
    let Some(minutes) = minutes(q.minutes, 60.0) else {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    };
    let q = sqlx::query_scalar(
        "SELECT json_build_object('type', 'FeatureCollection', 'features', coalesce(json_agg(json_build_object(
            'type', 'Feature',
            'geometry', ST_AsGeoJSON(geom, 6)::json,
            'properties', json_build_object(
                'name', name, 'callsign', callsign, 'kind', kind, 'time', time, 'direct', direct,
                'symbol', symbol, 'speed', speed, 'course', course, 'text', text, 'weather', weather))), '[]'))::text
         FROM aprs_station
         WHERE time > now() - make_interval(secs => $1 * 60)",
    )
    .bind(minutes);
    json(q, &app.db).await
}

/// Bulletins and weather-service alerts, newest first. Default: the last day.
async fn aprs_bulletins(State(app): State<App>, Query(q): Query<Since>) -> Result<Response, Error> {
    let Some(minutes) = minutes(q.minutes, 1440.0) else {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    };
    let q = sqlx::query_scalar(
        "SELECT coalesce(json_agg(b ORDER BY b.time DESC), '[]')::text
         FROM aprs_bulletin b
         WHERE time > now() - make_interval(secs => $1 * 60)",
    )
    .bind(minutes);
    json(q, &app.db).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn only_local_hosts_and_their_own_pages() {
        for host in ["localhost", "localhost:8000", "127.0.0.1", "192.168.4.1:80", "[::1]:8000", "carchomp", "carchomp.local", "pi.lan."] {
            assert!(trusted(Some(host), None), "{host}");
        }
        for host in ["evil.example.com", "192.168.4.1.nip.io", "", ":80"] {
            assert!(!trusted(Some(host), None), "{host}");
        }
        assert!(!trusted(None, None));
        assert!(trusted(Some("carchomp.local"), Some("http://carchomp.local")));
        assert!(trusted(Some("localhost:5173"), Some("http://localhost:5173")));
        assert!(!trusted(Some("192.168.4.1"), Some("https://evil.example.com")));
        assert!(!trusted(Some("localhost:8000"), Some("http://localhost:5173")));
        assert!(!trusted(Some("localhost"), Some("null")));
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn cidr_parsing() {
        assert_eq!(Net::parse("10.42.0.0/24"), Some(Net { addr: ip("10.42.0.0"), prefix: 24 }));
        assert_eq!(Net::parse(" 192.168.1.5 "), Some(Net { addr: ip("192.168.1.5"), prefix: 32 }));
        assert_eq!(Net::parse("fd00::/8"), Some(Net { addr: ip("fd00::"), prefix: 8 }));
        assert_eq!(Net::parse("::ffff:10.0.0.0/104"), Some(Net { addr: ip("10.0.0.0"), prefix: 8 }));
        for bad in ["::ffff:10.0.0.0/64", "", "10.42.0.0/33", "fd00::/129", "10.42.0/24", "10.42.0.0/", "/24", "10.42.0.0/-1", "host.local/24"] {
            assert_eq!(Net::parse(bad), None, "{bad}");
        }
        assert!(networks(&["10.42.0.0/24".into(), "fd00::/8".into()]).is_ok());
        assert!(networks(&["10.42.0.0/24".into(), "nope".into()]).unwrap_err().contains("\"nope\""));
    }

    #[test]
    fn cidr_matching() {
        let hotspot = Net::parse("10.42.0.0/24").unwrap();
        assert!(hotspot.contains(ip("10.42.0.1")));
        assert!(hotspot.contains(ip("10.42.0.255")));
        assert!(!hotspot.contains(ip("10.42.1.1")));
        assert!(!hotspot.contains(ip("10.43.0.1")));
        assert!(hotspot.contains(ip("::ffff:10.42.0.7")), "IPv4-mapped peers are unmapped");
        assert!(!hotspot.contains(ip("fd00::1")));
        let odd = Net::parse("192.168.4.128/25").unwrap();
        assert!(odd.contains(ip("192.168.4.200")) && !odd.contains(ip("192.168.4.127")));
        assert!(Net::parse("0.0.0.0/0").unwrap().contains(ip("8.8.8.8")));
        assert!(!Net::parse("0.0.0.0/0").unwrap().contains(ip("2001:db8::1")));
        assert!(Net::parse("::/0").unwrap().contains(ip("2001:db8::1")));
        let v6 = Net::parse("2001:db8:1::/48").unwrap();
        assert!(v6.contains(ip("2001:db8:1:ffff::1")) && !v6.contains(ip("2001:db8:2::1")));
        assert!(Net::parse("10.42.0.9").unwrap().contains(ip("10.42.0.9")));
        assert!(!Net::parse("10.42.0.9").unwrap().contains(ip("10.42.0.10")));
    }

    #[test]
    fn loopback_is_always_allowed() {
        let hotspot = networks(&["10.42.0.0/24".into()]).unwrap();
        for peer in ["127.0.0.1", "127.8.9.10", "::1", "::ffff:127.0.0.1", "10.42.0.23", "::ffff:10.42.0.23"] {
            assert!(peer_allowed(&hotspot, ip(peer)), "{peer}");
        }
        for peer in ["192.168.1.20", "10.42.1.2", "fe80::1", "::ffff:192.168.1.20"] {
            assert!(!peer_allowed(&hotspot, ip(peer)), "{peer}");
        }
        assert!(peer_allowed(&[], ip("::1")) && !peer_allowed(&[], ip("10.42.0.2")));
    }

    /// Through axum as main() wires it: the peer comes from ConnectInfo, and
    /// the fallback (the static UI) is covered like the API.
    #[tokio::test]
    async fn untrusted_peers_get_403_on_every_path() {
        use axum::extract::connect_info::MockConnectInfo;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let hotspot = Arc::new(networks(&["10.42.0.0/24".into()]).unwrap());
        for (peer, path, status) in [
            ("192.168.1.20:40000", "/api/health", "403"),
            ("192.168.1.20:40000", "/index.html", "403"),
            ("[fe80::1]:40000", "/api/health", "403"),
            ("10.42.0.7:40000", "/api/health", "200"),
            ("[::ffff:10.42.0.7]:40000", "/index.html", "200"),
            ("127.0.0.1:40000", "/api/health", "200"),
        ] {
            let app = Router::new()
                .route("/api/health", get(|| async { "ok" }))
                .fallback(|| async { "ui" })
                .layer(middleware::from_fn_with_state(hotspot.clone(), trusted_peer))
                .layer(MockConnectInfo(peer.parse::<SocketAddr>().unwrap()));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, app).await });
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with(&format!("HTTP/1.1 {status} ")), "{peer} {path}: {response}");
            assert_eq!(status == "403", response.contains("trusted_networks"), "{peer} {path}");
        }
    }

    #[test]
    fn floats_must_be_finite() {
        assert_eq!(floats::<3>("-122.6, 45.5,200"), Some([-122.6, 45.5, 200.0]));
        assert_eq!(floats::<3>("1,2"), None);
        for bad in ["nan,1,2", "inf,1,2", "1,-infinity,2", "1,2,x"] {
            assert_eq!(floats::<3>(bad), None, "{bad}");
        }
    }

    #[test]
    fn minutes_are_sane_spans() {
        assert_eq!(minutes(None, 60.0), Some(60.0));
        assert_eq!(minutes(Some(0.0), 60.0), Some(0.0));
        for bad in [f64::NAN, f64::INFINITY, -1.0, 1e12] {
            assert_eq!(minutes(Some(bad), 60.0), None, "{bad}");
        }
    }

    /// A 1 Hz drive east along the equator at `speed` m/s, `n` fixes.
    fn drive(n: i64, speed: f64) -> Vec<Fix> {
        let t = datetime!(2026-09-17 20:00 UTC);
        (0..n)
            .map(|i| Fix { lon: i as f64 * speed / 111_195.0, ..fix(t + Duration::seconds(i)) })
            .collect()
    }

    #[test]
    fn imports_are_thinned_like_live_drives() {
        // 30 m/s is above high_speed: one fix per fast_rate (15 s), plus the last.
        let kept: Vec<_> = thin(drive(61, 30.0), Params::default()).iter().map(|f| f.time.second()).collect();
        assert_eq!(kept, [0, 15, 30, 45, 0]);
        // What was kept is stored as it came: no derived speed or course.
        assert!(thin(drive(5, 30.0), Params::default()).iter().all(|f| f.speed.is_none() && f.course.is_none()));
    }

    #[test]
    fn imports_keep_their_corners() {
        // East for 20 s, then north for 20 s: the bend survives thinning.
        let mut fixes = drive(21, 20.0);
        let corner = fixes[20].clone();
        fixes.extend((1..=20).map(|i| Fix {
            time: corner.time + Duration::seconds(i),
            lat: i as f64 * 20.0 / 111_195.0,
            ..corner.clone()
        }));
        let kept = thin(fixes, Params::default());
        assert!(kept.iter().any(|f| f.time == corner.time), "the fix at the bend is kept");
        assert!(kept.len() < 10, "{} kept", kept.len());
    }

    #[test]
    fn distance_and_bearing() {
        let t = datetime!(2026-09-17 20:00 UTC);
        let a = fix(t);
        let north = Fix { lat: 1.0, ..fix(t) };
        let east = Fix { lon: 1.0, ..fix(t) };
        assert!((distance(&a, &north) - 111_195.0).abs() < 1.0);
        assert!((bearing(&a, &north) - 0.0).abs() < 1e-9);
        assert!((bearing(&a, &east) - 90.0).abs() < 1e-9);
        assert!((bearing(&east, &a) - 270.0).abs() < 1e-9);
    }

    #[test]
    fn export_is_one_gpx_with_a_trk_per_track() {
        let t = datetime!(2026-09-17 20:00 UTC);
        let p = |lon: f64| interchange::Point { lon, lat: 45.0, ele: None, time: Some(t) };
        let tracks = vec![(Some("Coast".to_owned()), vec![p(1.0), p(2.0)]), (None, vec![p(3.0), p(4.0)])];
        let gpx = gpx_all(&tracks);
        assert_eq!(gpx.matches("<gpx ").count(), 1);
        assert_eq!(gpx.matches("<trk>").count(), 2);
        let parsed = interchange::parse(&gpx).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name.as_deref(), Some("Coast"));
        assert_eq!(parsed[1].points, tracks[1].1);
        assert!(interchange::parse(&gpx_all(&[])).is_ok_and(|t| t.is_empty()));
    }

    fn fix(time: OffsetDateTime) -> Fix {
        Fix { time, lat: 0.0, lon: 0.0, alt: None, speed: None, course: None, h_err: None }
    }

    #[test]
    fn repeated_times_are_kept_apart_in_order() {
        let t = datetime!(2026-09-17 20:00:01 UTC);
        let us = Duration::microseconds(1);
        let mut fixes: Vec<_> = [t, t, t, t + us, t + Duration::seconds(1)].into_iter().map(fix).collect();
        distinct_times(&mut fixes);
        let got: Vec<_> = fixes.iter().map(|f| f.time).collect();
        assert_eq!(got, [t, t + us, t + 2 * us, t + 3 * us, t + Duration::seconds(1)]);
    }

    #[test]
    fn times_are_cut_to_microseconds_before_comparing() {
        let t = datetime!(2026-09-17 20:00:01.000000100 UTC);
        let mut fixes = vec![fix(t), fix(t + Duration::nanoseconds(200))];
        distinct_times(&mut fixes);
        assert_eq!(fixes[0].time, datetime!(2026-09-17 20:00:01 UTC));
        assert_eq!(fixes[1].time, datetime!(2026-09-17 20:00:01.000001 UTC));
    }
}
