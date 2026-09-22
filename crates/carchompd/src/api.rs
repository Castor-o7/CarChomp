//! HTTP surface: one WebSocket for everything live, a little REST for
//! everything stored. PostgreSQL builds the JSON, so there are no row structs
//! to keep in step with the schema.

use crate::{Bus, Config, Status, recorder, system};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use carchomp_core::{Fix, interchange};
use serde::Deserialize;
use sqlx::PgPool;
use std::path::PathBuf;
use time::{Duration, OffsetDateTime};
use tokio::sync::{broadcast::error::RecvError, watch};
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
struct App {
    db: PgPool,
    bus: Bus,
    status: watch::Sender<Status>,
    /// Where the bulk data lives; its disk is the one worth watching.
    data_dir: PathBuf,
}

pub fn router(db: PgPool, bus: Bus, status: watch::Sender<Status>, config: &Config) -> Router {
    let router = Router::new()
        .route("/ws", get(ws))
        .route("/api/health", get(health))
        .route("/api/tracks", get(list_tracks))
        .route("/api/tracks/import", post(import_tracks).layer(DefaultBodyLimit::max(64 << 20)))
        .route("/api/tracks/{id}", get(get_track).patch(patch_track).delete(delete_track))
        .route("/api/tracks/{id}/gpx", get(get_track_gpx))
        .route("/api/segments", get(segments))
        .route("/api/aprs/stations", get(aprs_stations))
        .route("/api/aprs/bulletins", get(aprs_bulletins))
        // Without this, a mistyped API path would be answered with the UI.
        .route("/api/{*unknown}", any(|| async { StatusCode::NOT_FOUND }))
        .with_state(App { db, bus, status, data_dir: config.maps_dir.clone() });
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
            e => {
                tracing::error!("api: {e}");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

/// Run a query that returns a single JSON text value and pass it through.
async fn json(query: sqlx::query::QueryScalar<'_, sqlx::Postgres, String, sqlx::postgres::PgArguments>, db: &PgPool) -> Result<Response, Error> {
    let body = query.fetch_one(db).await?;
    Ok(([(header::CONTENT_TYPE, "application/json")], body).into_response())
}

async fn ws(State(app): State<App>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(|socket| live(socket, app))
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
                Some(Ok(_)) => continue,
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
            'fixes', (SELECT count(*) FROM fix),
            'aprs_stations', (SELECT count(DISTINCT callsign) FROM aprs_packet),
            'database_bytes', pg_database_size(current_database()))::text",
    )
    .bind(env!("CARGO_PKG_VERSION"));
    let body: String = q.fetch_one(&app.db).await?;
    let mut health: serde_json::Value = serde_json::from_str(&body).expect("PostgreSQL writes valid JSON");
    health["system"] = system::snapshot(&app.data_dir);
    Ok(Json(health).into_response())
}

#[derive(Deserialize)]
struct Near {
    /// `lon,lat,metres`: only tracks passing within that distance.
    near: Option<String>,
}

async fn list_tracks(State(app): State<App>, Query(q): Query<Near>) -> Result<Response, Error> {
    let (lon, lat, r) = match q.near.as_deref().map(floats) {
        Some(Some([lon, lat, r])) => (Some(lon), Some(lat), Some(r)),
        Some(None) => return Ok(StatusCode::BAD_REQUEST.into_response()),
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
async fn import_tracks(State(app): State<App>, body: String) -> Result<Response, Error> {
    let tracks = match interchange::parse(&body) {
        Ok(tracks) if tracks.is_empty() => return Ok((StatusCode::BAD_REQUEST, "no tracks found").into_response()),
        Ok(tracks) => tracks,
        Err(e) => return Ok((StatusCode::BAD_REQUEST, e).into_response()),
    };
    let source = recorder::source_id(&app.db, "import", "upload").await?;
    let now = OffsetDateTime::now_utc();
    let mut ids = Vec::new();
    for track in tracks {
        // Time is part of a fix's identity. Points that come without one get a
        // second apiece, counted from the moment of import.
        let timed = track.points.iter().all(|p| p.time.is_some());
        let fixes: Vec<Fix> = (0..)
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
        let id: i64 = sqlx::query_scalar("INSERT INTO track (name, started) VALUES ($1, $2) RETURNING id")
            .bind(&track.name)
            .bind(fixes[0].time)
            .fetch_one(&app.db)
            .await?;
        recorder::insert_fixes(&app.db, source, id, &fixes).await?;
        recorder::finish_track(&app.db, id).await?;
        ids.push(id);
    }
    Ok(Json(ids).into_response())
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
    let v: Vec<f64> = s.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>().ok()?;
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

async fn delete_track(State(app): State<App>, Path(id): Path<i64>) -> Result<StatusCode, Error> {
    let done = sqlx::query("DELETE FROM track WHERE id = $1").bind(id).execute(&app.db).await?;
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

/// Stations and objects on the map, as a GeoJSON FeatureCollection. Default: the last hour.
async fn aprs_stations(State(app): State<App>, Query(q): Query<Since>) -> Result<Response, Error> {
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
    .bind(q.minutes.unwrap_or(60.0));
    json(q, &app.db).await
}

/// Bulletins and weather-service alerts, newest first. Default: the last day.
async fn aprs_bulletins(State(app): State<App>, Query(q): Query<Since>) -> Result<Response, Error> {
    let q = sqlx::query_scalar(
        "SELECT coalesce(json_agg(b ORDER BY b.time DESC), '[]')::text
         FROM aprs_bulletin b
         WHERE time > now() - make_interval(secs => $1 * 60)",
    )
    .bind(q.minutes.unwrap_or(1440.0));
    json(q, &app.db).await
}
