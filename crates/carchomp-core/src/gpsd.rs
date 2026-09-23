//! The slice of gpsd's JSON protocol we need.

use crate::Fix;
use serde::Deserialize;
use time::OffsetDateTime;

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
    Some(Fix {
        time: t.time?,
        lat: t.lat?,
        lon: t.lon?,
        alt: t.alt_hae,
        speed: t.speed,
        course: t.track,
        h_err: t.eph,
    })
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
    fn non_finite_fields_are_dropped_not_the_fix() {
        let line = r#"{"class":"TPV","mode":3,"time":"2026-09-17T20:00:00.000Z","lat":45.5,"lon":-122.6,"climb":nan,"track":-inf,"speed":1.5}"#;
        let fix = parse_fix(line).unwrap();
        assert_eq!(fix.course, None);
        assert_eq!(fix.speed, Some(1.5));
    }
}
