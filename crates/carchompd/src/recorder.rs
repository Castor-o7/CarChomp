//! The only writer. Listens to the bus, decides what is worth keeping, and
//! owns the track lifecycle: a track starts when we move, ends when we have
//! been still (or silent) for `track_idle`, and is then cut into indexed
//! segments so later drives can ask "have I been here?".

use crate::{Bus, Config, Event, Status};
use carchomp_core::{
    Fix, Observation, aprs,
    beacon::{Params, SmartBeacon},
};
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio::{
    sync::{broadcast::error::RecvError, watch},
    time::Instant,
};

pub async fn source_id(db: &PgPool, kind: &str, name: &str) -> sqlx::Result<i16> {
    // Look before inserting: an INSERT takes an id from the (smallint)
    // sequence even when it then conflicts, and this runs on every boot.
    let find = || sqlx::query_scalar("SELECT id FROM source WHERE kind = $1 AND name = $2").bind(kind).bind(name);
    if let Some(id) = find().fetch_optional(db).await? {
        return Ok(id);
    }
    let id = sqlx::query_scalar("INSERT INTO source (kind, name) VALUES ($1, $2) ON CONFLICT DO NOTHING RETURNING id")
        .bind(kind)
        .bind(name)
        .fetch_optional(db)
        .await?;
    match id {
        Some(id) => Ok(id),
        None => find().fetch_one(db).await, // inserted by someone else meanwhile
    }
}

/// Has an earlier track used the road we are on? "Used" means one of its
/// edges passes within $4 metres *and* runs our way (or the opposite way) to
/// within $6 degrees, so crossing an old track at an intersection, or passing
/// under one on a bridge, does not count. With no course ($5 null, as when
/// barely moving) nearness alone decides. The index finds the few nearby
/// segments; only those are taken apart into edges.
const KNOWN_ROAD: &str = "
    SELECT EXISTS (
        SELECT 1
        FROM track_segment s, LATERAL ST_DumpSegments(s.geom::geometry) AS edge
        WHERE s.track_id <> $1
          AND ST_DWithin(s.geom, ST_MakePoint($2, $3)::geography, $4)
          AND ST_DWithin(edge.geom::geography, ST_MakePoint($2, $3)::geography, $4)
          AND ($5::float8 IS NULL OR $6 >= abs(90 - mod((3690 + $5 - degrees(
                  ST_Azimuth(ST_StartPoint(edge.geom)::geography, ST_EndPoint(edge.geom)::geography)))::numeric, 180))))";

struct Recorder {
    db: PgPool,
    bus: Bus,
    params: Params,
    track_idle: f64,
    road_radius: f64,
    road_heading: f64,
    beacon: SmartBeacon,
    status: watch::Sender<Status>,
    last_moving: Option<OffsetDateTime>,
    last_road_check: Option<OffsetDateTime>,
    /// When the open track ends if nothing moving is heard, by the local
    /// clock: fixes may stop coming at all (receiver unplugged).
    idle_deadline: Option<Instant>,
    /// Source of the latest fix, for storing the one the beacon skipped.
    last_source: Option<i16>,
}

/// Seconds between "known road?" checks, whether or not fixes are stored.
const ROAD_CHECK: f64 = 3.0;

fn road_check_due(last: Option<OffsetDateTime>, now: OffsetDateTime) -> bool {
    last.is_none_or(|t| (now - t).as_seconds_f64().abs() >= ROAD_CHECK)
}

pub fn run(db: PgPool, bus: Bus, status: watch::Sender<Status>, config: &Config) -> impl Future<Output = ()> + use<> {
    let mut rec = Recorder {
        db,
        bus,
        params: config.beacon,
        track_idle: config.track_idle,
        road_radius: config.road_radius,
        road_heading: config.road_heading,
        beacon: SmartBeacon::new(config.beacon),
        status,
        last_moving: None,
        last_road_check: None,
        idle_deadline: None,
        last_source: None,
    };
    async move {
        let mut events = rec.bus.subscribe();
        if let Err(e) = rec.close_abandoned_tracks().await {
            tracing::error!("closing abandoned tracks: {e}");
        }
        loop {
            let deadline = rec.idle_deadline;
            let result = tokio::select! {
                event = events.recv() => match event {
                    Ok(Event { source, obs: Observation::Fix(fix) }) => rec.fix(source, &fix).await,
                    Ok(Event { source, obs: Observation::Aprs(p) }) => rec.aprs(source, &p).await,
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!("recorder fell behind, dropped {n} events");
                        Ok(())
                    }
                    Err(RecvError::Closed) => return,
                },
                () = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                    rec.last_moving = None;
                    rec.end_track().await
                }
            };
            // A database hiccup must not stop recording for the rest of the drive.
            if let Err(e) = result {
                tracing::error!("recorder: {e}");
            }
        }
    }
}

impl Recorder {
    async fn fix(&mut self, source: i16, fix: &Fix) -> sqlx::Result<()> {
        let moving = fix.speed.unwrap_or(0.0) >= self.params.low_speed;
        let idle = self
            .last_moving
            .is_some_and(|t| (fix.time - t).as_seconds_f64() > self.track_idle);
        if moving {
            self.last_moving = Some(fix.time);
            self.idle_deadline = std::time::Duration::try_from_secs_f64(self.track_idle.max(0.0))
                .ok()
                .and_then(|d| Instant::now().checked_add(d));
        }
        self.last_source = Some(source);
        if idle {
            self.end_track().await?;
        }
        let current = self.status.borrow().track;
        let track = match current {
            Some(track) => track,
            None if moving => self.start_track(fix.time).await?,
            None => return Ok(()),
        };

        let kept = self.beacon.push(fix);
        if !kept.is_empty() {
            insert_fixes(&self.db, source, track, &kept).await?;
        }

        // Checked every few seconds rather than at the storage rate, which
        // can be two minutes when slow: a gentle fork onto a new road must
        // not wait for the next stored fix.
        if !road_check_due(self.last_road_check, fix.time) {
            return Ok(());
        }
        self.last_road_check = Some(fix.time);
        let known: bool = sqlx::query_scalar(KNOWN_ROAD)
            .bind(track)
            .bind(fix.lon)
            .bind(fix.lat)
            .bind(self.road_radius)
            .bind(fix.course.filter(|_| moving))
            .bind(self.road_heading)
            .fetch_one(&self.db)
            .await?;
        if self.set(|s| s.road_new = Some(!known)) {
            tracing::info!("{} road", if known { "known" } else { "new" });
        }
        Ok(())
    }

    async fn aprs(&mut self, source: i16, p: &aprs::Packet) -> sqlx::Result<()> {
        let pos = p.position.as_ref();
        let weather = p.weather.as_ref().map(|w| serde_json::to_string(w).expect("weather serializes"));
        sqlx::query(
            "INSERT INTO aprs_packet (source_id, callsign, direct, kind, name, alive, geom,
                                      symbol, speed, course, weather, addressee, text, raw)
             VALUES ($1, $2, $3, $4, $5, $6, ST_MakePoint($7, $8)::geography, $9, $10, $11, $12::jsonb, $13, $14, $15)",
        )
        .bind(source)
        .bind(&p.from)
        .bind(p.direct)
        .bind(p.kind.as_str())
        .bind(&p.name)
        .bind(p.alive)
        // ST_MakePoint is strict: NULL coordinates give a NULL geom.
        .bind(pos.map(|p| p.lon))
        .bind(pos.map(|p| p.lat))
        .bind(pos.map(|p| &p.symbol))
        .bind(pos.and_then(|p| p.speed).map(|v| v as f32))
        .bind(pos.and_then(|p| p.course).map(|v| v as f32))
        .bind(weather)
        .bind(&p.addressee)
        .bind(&p.text)
        .bind(&p.raw)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// Update the published status; true if that changed anything.
    fn set(&self, change: impl FnOnce(&mut Status)) -> bool {
        self.status.send_if_modified(|status| {
            let before = *status;
            change(status);
            *status != before
        })
    }

    async fn start_track(&mut self, at: OffsetDateTime) -> sqlx::Result<i64> {
        let id = sqlx::query_scalar("INSERT INTO track (started) VALUES ($1) RETURNING id")
            .bind(at)
            .fetch_one(&self.db)
            .await?;
        tracing::info!("track {id} started");
        self.beacon.reset();
        self.set(|s| *s = Status { track: Some(id), road_new: None });
        Ok(id)
    }

    async fn end_track(&mut self) -> sqlx::Result<()> {
        self.idle_deadline = None;
        let track = self.status.borrow().track;
        if let Some(id) = track {
            // End it where we last were, not at the last fix worth keeping.
            let last = self.beacon.take_skipped();
            let stored = match (last, self.last_source) {
                (Some(fix), Some(source)) => insert_fixes(&self.db, source, id, &[fix]).await,
                _ => Ok(()),
            };
            // Cleared even if the database fails, so the track is picked up
            // as abandoned at the next start rather than retried every fix;
            // but only once it is stored, so clients reload a finished track.
            let finished = finish_track(&self.db, id).await;
            self.set(|s| *s = Status::default());
            stored.and(finished)?;
            tracing::info!("track {id} ended");
        }
        Ok(())
    }

    /// Tracks left open by a power cut (the normal way a car computer stops).
    async fn close_abandoned_tracks(&self) -> sqlx::Result<()> {
        let open: Vec<i64> = sqlx::query_scalar("SELECT id FROM track WHERE ended IS NULL")
            .fetch_all(&self.db)
            .await?;
        for id in open {
            finish_track(&self.db, id).await?;
        }
        Ok(())
    }
}

pub async fn insert_fixes(db: &PgPool, source: i16, track: i64, fixes: &[Fix]) -> sqlx::Result<()> {
    let column = |f: fn(&Fix) -> Option<f64>| fixes.iter().map(|x| f(x).map(|v| v as f32)).collect::<Vec<_>>();
    sqlx::query(
        "INSERT INTO fix (time, source_id, track_id, geom, alt, speed, course, h_err)
         SELECT time, $1, $2, ST_SetSRID(ST_MakePoint(lon, lat), 4326), alt, speed, course, h_err
         FROM unnest($3::timestamptz[], $4::float8[], $5::float8[], $6::real[], $7::real[], $8::real[], $9::real[])
              AS t (time, lon, lat, alt, speed, course, h_err)
         ON CONFLICT DO NOTHING",
    )
    .bind(source)
    .bind(track)
    .bind(fixes.iter().map(|f| f.time).collect::<Vec<_>>())
    .bind(fixes.iter().map(|f| f.lon).collect::<Vec<_>>())
    .bind(fixes.iter().map(|f| f.lat).collect::<Vec<_>>())
    .bind(column(|f| f.alt))
    .bind(column(|f| f.speed))
    .bind(column(|f| f.course))
    .bind(column(|f| f.h_err))
    .execute(db)
    .await?;
    Ok(())
}

const MIN_TRACK: f64 = 50.0;

/// Close a track and index its geometry. Tracks shorter than `MIN_TRACK`
/// metres (a GPS glitch while parked) are dropped. Safe to repeat: the
/// segments are rebuilt from the fixes. Also used by import.
pub async fn finish_track(db: &PgPool, id: i64) -> sqlx::Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM track_segment WHERE track_id = $1").bind(id).execute(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO track_segment (track_id, geom)
         SELECT $1, ST_Subdivide(line, 32)::geography
         FROM (SELECT ST_MakeLine(geom ORDER BY time) AS line
               FROM fix WHERE track_id = $1) t
         WHERE ST_Length(line::geography) >= $2",
    )
    .bind(id)
    .bind(MIN_TRACK)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE track SET ended = (SELECT max(time) FROM fix WHERE track_id = $1) WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM track WHERE id = $1 AND NOT EXISTS (SELECT 1 FROM track_segment WHERE track_id = $1)")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    #[test]
    fn road_check_cadence() {
        let t = OffsetDateTime::UNIX_EPOCH;
        assert!(road_check_due(None, t));
        assert!(!road_check_due(Some(t), t + Duration::seconds(1)));
        assert!(road_check_due(Some(t), t + Duration::seconds(3)));
    }
}
