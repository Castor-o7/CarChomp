//! APRS SmartBeaconing, used as a storage policy instead of a transmit policy.
//!
//! The original algorithm (Tony Arnerich KD7TA & Steve Bragg KA9MVA, HamHUD)
//! decides when a moving station should report: rarely when stopped, more
//! often the faster it moves, and at once when it turns a corner. Here "report"
//! means "write this fix to the database", so straight roads cost few rows and
//! bends are kept.

use crate::{Fix, heading_diff};
use serde::Deserialize;

/// Units are SI: metres per second, seconds, degrees.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Params {
    /// Below this we are "stopped": keep the fix where we stop, then one every
    /// `slow_rate`, and ignore turns (GPS course is noise at walking pace).
    pub low_speed: f64,
    /// At or above this, keep a fix every `fast_rate`.
    pub high_speed: f64,
    pub slow_rate: f64,
    pub fast_rate: f64,
    /// Minimum heading change that counts as a corner at high speed.
    pub turn_min: f64,
    /// Added to `turn_min` as `turn_slope / speed`: slow vehicles must turn
    /// further before it counts.
    pub turn_slope: f64,
    /// Minimum time between corner-triggered fixes.
    pub turn_time: f64,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            low_speed: 2.0,
            high_speed: 27.0,
            slow_rate: 120.0,
            fast_rate: 15.0,
            turn_min: 10.0,
            turn_slope: 110.0,
            turn_time: 3.0,
        }
    }
}

#[derive(Debug, Default)]
pub struct SmartBeacon {
    params: Params,
    /// Time (unix seconds), course, and whether we were moving, at the last
    /// kept fix.
    last: Option<(f64, Option<f64>, bool)>,
    /// The most recent fix, if it was not kept.
    skipped: Option<Fix>,
}

enum Decision {
    Skip,
    Keep,
    Corner,
}

impl SmartBeacon {
    pub fn new(params: Params) -> Self {
        Self { params, ..Self::default() }
    }

    /// Forget everything, so the next fix is kept unconditionally.
    pub fn reset(&mut self) {
        self.last = None;
        self.skipped = None;
    }

    /// Feed the next fix; returns the fixes worth storing. Usually none or
    /// one. A corner yields two: a turn is only visible once we are past it,
    /// so the fix from just before it is kept as well, or the stored line
    /// would cut the corner.
    pub fn push(&mut self, fix: &Fix) -> Vec<Fix> {
        let before = self.skipped.replace(fix.clone());
        let kept = match self.decide(fix) {
            Decision::Skip => return Vec::new(),
            Decision::Keep => vec![fix.clone()],
            Decision::Corner => before.into_iter().chain([fix.clone()]).collect(),
        };
        let moving = fix.speed.unwrap_or(0.0) >= self.params.low_speed;
        self.last = Some((seconds(fix), fix.course, moving));
        self.skipped = None;
        kept
    }

    /// The most recent fix if it was not kept, so a track that ends can still
    /// be stored up to where we last were.
    pub fn take_skipped(&mut self) -> Option<Fix> {
        self.skipped.take()
    }

    fn decide(&self, fix: &Fix) -> Decision {
        let p = &self.params;
        let Some((last_time, last_course, was_moving)) = self.last else {
            return Decision::Keep;
        };
        let elapsed = seconds(fix) - last_time;
        // The source's clock stepped back (restarted, or corrected itself):
        // start over from here rather than wait for it to catch up.
        if elapsed < 0.0 {
            return Decision::Keep;
        }
        let speed = fix.speed.unwrap_or(0.0);

        let rate = if speed < p.low_speed {
            // Where we come to rest is where the drive ends: keep it now,
            // since the power may be switched off in a moment.
            if was_moving {
                return Decision::Keep;
            }
            p.slow_rate
        } else {
            if let (Some(now), Some(then)) = (fix.course, last_course) {
                let threshold = p.turn_min + p.turn_slope / speed;
                if elapsed >= p.turn_time && heading_diff(now, then) > threshold {
                    return Decision::Corner;
                }
            }
            // max/min rather than clamp: clamp panics if the config has them reversed.
            (p.fast_rate * p.high_speed / speed).max(p.fast_rate).min(p.slow_rate)
        };
        if elapsed >= rate { Decision::Keep } else { Decision::Skip }
    }
}

fn seconds(fix: &Fix) -> f64 {
    fix.time.unix_timestamp_nanos() as f64 / 1e9
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::{Duration, OffsetDateTime};

    fn fix(t: i64, speed: f64, course: f64) -> Fix {
        Fix {
            time: OffsetDateTime::UNIX_EPOCH + Duration::seconds(t),
            lat: 45.5,
            lon: -122.6,
            alt: None,
            speed: Some(speed),
            course: Some(course),
            h_err: None,
        }
    }

    /// Feed 1 Hz fixes and return the seconds at which one was kept.
    fn run(fixes: impl Iterator<Item = Fix>) -> Vec<i64> {
        let mut sb = SmartBeacon::default();
        fixes
            .flat_map(|f| sb.push(&f))
            .map(|f| f.time.unix_timestamp())
            .collect()
    }

    #[test]
    fn first_fix_is_always_kept() {
        assert_eq!(run([fix(0, 0.0, 0.0)].into_iter()), [0]);
    }

    #[test]
    fn highway_keeps_one_fix_per_fast_rate() {
        let kept = run((0..=60).map(|t| fix(t, 30.0, 90.0)));
        assert_eq!(kept, [0, 15, 30, 45, 60]);
    }

    #[test]
    fn slower_means_sparser() {
        // 13.5 m/s is half of high_speed, so the interval doubles.
        let kept = run((0..=60).map(|t| fix(t, 13.5, 90.0)));
        assert_eq!(kept, [0, 30, 60]);
    }

    #[test]
    fn stopped_uses_slow_rate_and_ignores_course_noise() {
        let kept = run((0..=240).map(|t| fix(t, 0.3, (t * 97 % 360) as f64)));
        assert_eq!(kept, [0, 120, 240]);
    }

    #[test]
    fn stop_is_kept() {
        let kept = run((0..=60).map(|t| fix(t, if t < 10 { 30.0 } else { 0.3 }, 90.0)));
        assert_eq!(kept, [0, 10]);
        // Stop-and-go: only the first stop after a kept moving fix is pegged.
        let kept = run((0..=60).map(|t| fix(t, if t < 20 { 13.0 } else if t % 2 == 0 { 0.3 } else { 2.1 }, 90.0)));
        assert_eq!(kept, [0, 20]);
    }

    #[test]
    fn clock_stepping_back_starts_over() {
        let kept = run([fix(100, 30.0, 90.0), fix(50, 30.0, 90.0), fix(51, 30.0, 90.0), fix(65, 30.0, 90.0)].into_iter());
        assert_eq!(kept, [100, 50, 65]);
    }

    #[test]
    fn last_skipped_fix_can_be_taken() {
        let mut sb = SmartBeacon::default();
        for t in 0..5 {
            sb.push(&fix(t, 0.3, 0.0));
        }
        assert_eq!(sb.take_skipped().map(|f| f.time.unix_timestamp()), Some(4));
        assert!(sb.take_skipped().is_none());
    }

    #[test]
    fn corner_is_pegged() {
        // East for 5 s, then a 90 degree turn north: keeps both sides of it.
        let kept = run((0..10).map(|t| fix(t, 15.0, if t < 5 { 90.0 } else { 0.0 })));
        assert_eq!(kept, [0, 4, 5]);
    }

    #[test]
    fn gentle_curve_is_not_a_corner() {
        // threshold at 15 m/s = 10 + 110/15 = 17.3 degrees
        let kept = run((0..10).map(|t| fix(t, 15.0, 90.0 + t as f64)));
        assert_eq!(kept, [0]);
    }

    #[test]
    fn misspelt_param_is_rejected() {
        assert!(serde_json::from_str::<Params>(r#"{"fastrate": 5}"#).is_err());
        assert_eq!(serde_json::from_str::<Params>(r#"{"fast_rate": 5}"#).unwrap().fast_rate, 5.0);
    }

    #[test]
    fn reversed_or_nan_rates_do_not_panic() {
        for (fast, slow) in [(60.0, 30.0), (f64::NAN, 120.0), (15.0, f64::NAN)] {
            let mut sb = SmartBeacon::new(Params { fast_rate: fast, slow_rate: slow, ..Params::default() });
            for t in 0..=200 {
                sb.push(&fix(t, 30.0, 90.0));
            }
        }
    }
}
