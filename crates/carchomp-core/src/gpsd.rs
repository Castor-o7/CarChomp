//! The slice of gpsd's JSON protocol we need.

use crate::Fix;
use serde::Deserialize;
use time::{Duration, OffsetDateTime, macros::datetime};

/// Sent once after connecting to start the report stream.
pub const WATCH: &str = "?WATCH={\"enable\":true,\"json\":true};\n";

#[derive(Debug, Deserialize)]
#[serde(tag = "class")]
pub enum Report {
    TPV(Tpv),
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub struct Tpv {
    /// 0/1 = no fix, 2 = 2D, 3 = 3D.
    #[serde(default)]
    pub mode: u8,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub time: Option<OffsetDateTime>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    #[serde(rename = "altHAE")]
    pub alt_hae: Option<f64>,
    pub speed: Option<f64>,
    pub track: Option<f64>,
    pub eph: Option<f64>,
}

/// No fix we receive can be older than this software. Anything earlier is a
/// receiver whose 10-bit GPS week counter has wrapped (dated 1024 weeks ago),
/// or one that has not worked out the date yet. The Pi has no clock to
/// compare with, so the floor is fixed.
const EARLIEST: OffsetDateTime = datetime!(2026-01-01 0:00 UTC);
const GPS_WEEK_ROLLOVER: Duration = Duration::weeks(1024);

/// Parse one line from gpsd. `None` for anything that is not a usable fix.
/// Some gpsd/driver combinations leak C `nan`/`inf` into the JSON; those
/// fields are treated as absent rather than discarding the whole fix.
pub fn parse_fix(line: &str) -> Option<Fix> {
    let report = serde_json::from_str(line).or_else(|_| serde_json::from_str(&without_non_finite(line)));
    let Ok(Report::TPV(t)) = report else {
        return None;
    };
    if t.mode < 2 {
        return None;
    }
    let time = match t.time? {
        time if time >= EARLIEST => time,
        time => Some(time + GPS_WEEK_ROLLOVER).filter(|&t| t >= EARLIEST)?,
    };
    Some(Fix {
        time,
        lat: t.lat?,
        lon: t.lon?,
        alt: t.alt_hae,
        speed: t.speed,
        course: t.track,
        h_err: t.eph,
    })
}

/// gpsd may report one epoch in several TPVs as NMEA sentences arrive: one
/// with the position only (GGA), then one with speed and course (RMC). A
/// missing speed would read as "stopped", so while the receiver is known to
/// report speed, fixes without one are partial and should be ignored.
/// Receivers that never report speed still get through.
#[derive(Debug, Default)]
pub struct Partials {
    last_speed: Option<OffsetDateTime>,
}

impl Partials {
    pub fn is_partial(&mut self, fix: &Fix) -> bool {
        if fix.speed.is_some() {
            self.last_speed = Some(fix.time);
            return false;
        }
        self.last_speed.is_some_and(|t| (fix.time - t).abs() <= Duration::seconds(2))
    }
}

fn without_non_finite(line: &str) -> String {
    ["-nan", "-inf", "nan", "inf"].iter().fold(line.to_owned(), |s, t| s.replace(&format!(":{t}"), ":null"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_3d_fix() {
        let line = r#"{"class":"TPV","device":"/dev/ttyACM0","mode":3,"time":"2026-09-17T20:00:00.000Z","lat":45.5122,"lon":-122.6587,"altHAE":15.2,"speed":13.4,"track":271.5,"eph":4.1}"#;
        let fix = parse_fix(line).unwrap();
        assert_eq!(fix.lat, 45.5122);
        assert_eq!(fix.course, Some(271.5));
        assert_eq!(fix.time.unix_timestamp(), 1_789_675_200);
    }

    #[test]
    fn ignores_everything_else() {
        assert!(parse_fix(r#"{"class":"SKY","satellites":[]}"#).is_none());
        assert!(parse_fix(r#"{"class":"TPV","mode":1}"#).is_none());
        assert!(parse_fix("garbage").is_none());
    }

    #[test]
    fn week_rollover_is_undone_and_nonsense_dates_dropped() {
        let at = |time: &str| parse_fix(&format!(r#"{{"class":"TPV","mode":3,"time":"{time}","lat":45.5,"lon":-122.6}}"#));
        // 2007-02-05 + 1024 weeks.
        assert_eq!(at("2007-02-05T12:00:00Z").unwrap().time, datetime!(2026-09-21 12:00 UTC));
        assert!(at("1980-01-06T00:00:00Z").is_none());
        assert_eq!(at("2026-09-17T20:00:00Z").unwrap().time, datetime!(2026-09-17 20:00 UTC));
    }

    #[test]
    fn partial_tpv_is_ignored_while_speed_is_reported() {
        let gga = parse_fix(r#"{"class":"TPV","mode":3,"time":"2026-09-17T20:00:00Z","lat":45.5,"lon":-122.6}"#).unwrap();
        let rmc = parse_fix(r#"{"class":"TPV","mode":3,"time":"2026-09-17T20:00:00Z","lat":45.5,"lon":-122.6,"speed":25.0,"track":90.0}"#).unwrap();
        let mut p = Partials::default();
        // Until speed has been seen, a fix without it may be all there is.
        assert!(!p.is_partial(&gga));
        assert!(!p.is_partial(&rmc));
        assert!(p.is_partial(&gga));
        let later = Fix { time: gga.time + Duration::seconds(10), ..gga };
        assert!(!p.is_partial(&later));
    }

    #[test]
    fn non_finite_fields_are_dropped_not_the_fix() {
        let line = r#"{"class":"TPV","mode":3,"time":"2026-09-17T20:00:00.000Z","lat":45.5,"lon":-122.6,"climb":nan,"track":-inf,"speed":1.5}"#;
        let fix = parse_fix(line).unwrap();
        assert_eq!(fix.course, None);
        assert_eq!(fix.speed, Some(1.5));
    }
}
