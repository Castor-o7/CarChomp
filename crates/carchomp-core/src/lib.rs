//! I/O-free carchomp logic. Everything here is synchronous and testable
//! without hardware, a database, or a network.

pub mod aprs;
pub mod beacon;
pub mod eapconfig;
pub mod gpsd;
pub mod interchange;
pub mod kiss;
pub mod nm;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Knots to metres per second.
pub const KNOT: f64 = 0.514_444;

/// Our own position, from any positioning source (GPS, RF, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fix {
    #[serde(with = "time::serde::rfc3339")]
    pub time: OffsetDateTime,
    pub lat: f64,
    pub lon: f64,
    /// Metres above the WGS84 ellipsoid.
    pub alt: Option<f64>,
    /// Metres per second over ground.
    pub speed: Option<f64>,
    /// Degrees true, 0..360.
    pub course: Option<f64>,
    /// Estimated horizontal error in metres. This is what lets sources of
    /// very different quality share one table.
    pub h_err: Option<f64>,
}

/// Anything a source can report. New source kinds add a variant here;
/// consumers that don't care about it ignore it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Observation {
    Fix(Fix),
    Aprs(Box<aprs::Packet>),
}

/// Smallest absolute difference between two headings, in degrees (0..=180).
pub fn heading_diff(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_diff_wraps() {
        assert_eq!(heading_diff(350.0, 10.0), 20.0);
        assert_eq!(heading_diff(10.0, 350.0), 20.0);
        assert_eq!(heading_diff(90.0, 270.0), 180.0);
    }
}
