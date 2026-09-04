//! 7-day rate-limit pace projection.
//!
//! Anthropic's 7-day cap is a rolling window. With `used_percentage` and
//! `resets_at` we know where you are in the window (frac_elapsed) and can
//! project where you'll land at the current rate: projected = used / frac.
//! Goal is to hit ~100% each week — too far above means you'll cap mid-week,
//! too far below means wasted headroom.

use crate::ansi;
use crate::input::RateLimitWindow;

const SEVEN_DAY_MS: i64 = 7 * 24 * 3600 * 1000;

/// Below this fraction the projection denominator is small enough that the
/// number swings wildly hour-to-hour. We still SHOW the projection (day-1
/// overspend is a real signal worth surfacing — a 50%-in-4h burst means
/// you'll cap by midweek) but renderers should mark it as volatile so the
/// reader knows not to over-index on the exact number.
pub const VOLATILE_FRAC_THRESHOLD: f64 = 0.10;

#[derive(Debug, Clone, Copy)]
pub struct Pace {
    pub used_pct: f64,
    /// Populated whenever we can compute frac_elapsed (i.e. resets_at is
    /// present and the window hasn't already passed). Use `is_volatile()`
    /// to decide whether to display this with a volatility marker.
    pub projected: Option<f64>,
    pub frac_elapsed: Option<f64>,
}

/// 1 day / 7 days. The 7d window enters its "recovery-imminent" phase when
/// frac_elapsed crosses this threshold (~85.7%).
pub const LAST_24H_FRAC: f64 = 6.0 / 7.0;

impl Pace {
    /// True when we're early enough in the window that the projection
    /// denominator is unstable. Renderers should mark the projection with
    /// a `~` prefix (or similar) so the reader doesn't trust the exact value.
    pub fn is_volatile(&self) -> bool {
        self.frac_elapsed
            .map(|f| f < VOLATILE_FRAC_THRESHOLD)
            .unwrap_or(false)
    }

    /// True when <24h remain until the 7d window resets. Renderers should
    /// surface a recovery countdown (`→22h`) so the user can see how close
    /// they are to a fresh budget — the actionable signal late in the week.
    pub fn in_last_24h(&self) -> bool {
        self.frac_elapsed
            .map(|f| f >= LAST_24H_FRAC)
            .unwrap_or(false)
    }
}

/// A window's `resets_at` as Unix milliseconds, whichever shape CC or the
/// OAuth endpoint sent (Unix seconds, Unix ms, or RFC 3339). `None` when
/// absent or unparseable.
pub fn reset_ms(window: &RateLimitWindow) -> Option<i64> {
    match window.resets_at.as_ref()? {
        serde_json::Value::Number(n) => {
            let v = n.as_f64()?;
            Some(if v < 1e12 { (v * 1000.0) as i64 } else { v as i64 })
        }
        serde_json::Value::String(s) => crate::format::parse_rfc3339_ms(s),
        _ => None,
    }
}

/// Pace of an arbitrary rolling window of `window_ms` length, at `now_ms`.
/// `seven_day_pace` is this with the 7-day length and the wall clock.
pub fn pace_at(window: &RateLimitWindow, window_ms: i64, now_ms: i64) -> Option<Pace> {
    let used_pct = window.used_percentage?;
    let mut pace = Pace { used_pct, projected: None, frac_elapsed: None };
    let Some(reset) = reset_ms(window) else { return Some(pace) };
    let frac = (now_ms - (reset - window_ms)) as f64 / window_ms as f64;
    if !(0.0..=1.0).contains(&frac) {
        return Some(pace);
    }
    pace.frac_elapsed = Some(frac);
    pace.projected = Some(used_pct / frac);
    Some(pace)
}

pub fn seven_day_pace(seven_day: &RateLimitWindow) -> Option<Pace> {
    let used_pct = seven_day.used_percentage?;

    let mut pace = Pace { used_pct, projected: None, frac_elapsed: None };

    let Some(reset_ms) = reset_ms(seven_day) else { return Some(pace); };

    let window_start = reset_ms - SEVEN_DAY_MS;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    let frac = (now_ms - window_start) as f64 / SEVEN_DAY_MS as f64;
    // Project from minute 1. Only suppress when the window has already passed
    // (frac > 1.0) or clock skew makes frac negative. Early-window numbers
    // are volatile but a day-1 overspend IS the signal we want to surface;
    // caller checks `is_volatile()` to decide on a marker.
    if !(0.0..=1.0).contains(&frac) {
        return Some(pace);
    }
    pace.frac_elapsed = Some(frac);
    pace.projected = Some(used_pct / frac);
    Some(pace)
}

/// Pace color — asymmetric 92-105% green band. Goal is 100%; tighter on
/// overpace (risk of mid-week cutoff) than on underpace (just lost headroom).
pub fn pace_color(projected: f64, frac_elapsed: f64) -> &'static str {
    if projected > 115.0 { return ansi::RED; }       // way over — will cap before EOW
    if projected > 105.0 { return ansi::YELLOW; }    // moderately over
    if projected >= 92.0 { return ansi::GREEN; }     // sweet spot
    // Underpace severity ratchets with how late in the week we are.
    if projected >= 80.0 {
        return if frac_elapsed >= 0.85 { ansi::YELLOW } else { ansi::DIM };
    }
    if frac_elapsed >= 0.70 { return ansi::RED; }
    if frac_elapsed >= 0.40 { return ansi::YELLOW; }
    ansi::DIM
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pace_color_green_band() {
        // Center of sweet spot
        assert_eq!(pace_color(100.0, 0.5), ansi::GREEN);
        assert_eq!(pace_color(92.0, 0.5), ansi::GREEN);
    }

    #[test]
    fn pace_color_overpace() {
        assert_eq!(pace_color(120.0, 0.5), ansi::RED);
        assert_eq!(pace_color(110.0, 0.5), ansi::YELLOW);
    }

    #[test]
    fn is_volatile_below_threshold() {
        let p = Pace { used_pct: 50.0, projected: Some(500.0), frac_elapsed: Some(0.05) };
        assert!(p.is_volatile(), "5% elapsed should be volatile");
        let p2 = Pace { used_pct: 50.0, projected: Some(100.0), frac_elapsed: Some(0.50) };
        assert!(!p2.is_volatile(), "50% elapsed should not be volatile");
    }

    #[test]
    fn in_last_24h_threshold() {
        let early = Pace { used_pct: 50.0, projected: Some(100.0), frac_elapsed: Some(0.5) };
        assert!(!early.in_last_24h(), "mid-week is not last 24h");
        let late = Pace { used_pct: 90.0, projected: Some(100.0), frac_elapsed: Some(6.5 / 7.0) };
        assert!(late.in_last_24h(), "12h before reset is last 24h");
        let boundary = Pace { used_pct: 86.0, projected: Some(100.0), frac_elapsed: Some(LAST_24H_FRAC) };
        assert!(boundary.in_last_24h(), "exactly at threshold counts as last 24h");
    }

    #[test]
    fn is_volatile_at_exact_threshold() {
        let p = Pace {
            used_pct: 50.0,
            projected: Some(500.0),
            frac_elapsed: Some(VOLATILE_FRAC_THRESHOLD),
        };
        assert!(!p.is_volatile(), "exactly at threshold is no longer volatile");
    }

    #[test]
    fn is_volatile_false_when_no_frac() {
        let p = Pace { used_pct: 50.0, projected: None, frac_elapsed: None };
        assert!(!p.is_volatile(), "no frac means we can't say it's volatile");
    }

    #[test]
    fn seven_day_pace_projects_from_day_one() {
        // Day-1 projection (frac ≈ 0.024 = ~4 hours into the week).
        // Reset is in 6.84 days from now → window_start ≈ 4h ago.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let reset_ms = now_ms + SEVEN_DAY_MS - 4 * 3600 * 1000;
        let rlw = RateLimitWindow {
            used_percentage: Some(2.0), // 2% used in ~4h
            resets_at: Some(serde_json::Value::Number(serde_json::Number::from(reset_ms / 1000))),
        };
        let p = seven_day_pace(&rlw).expect("returns Some");
        let projected = p.projected.expect("projection populated on day 1");
        // 2% / 0.024 ≈ 83%. Tolerant assertion — just want "in the ballpark".
        assert!(projected > 60.0 && projected < 110.0, "projection ~83%, got {projected}");
        assert!(p.is_volatile(), "day-1 marked volatile");
    }

    #[test]
    fn pace_color_underpace_escalates_with_time() {
        // 85% projected: dim early, yellow late
        assert_eq!(pace_color(85.0, 0.5), ansi::DIM);
        assert_eq!(pace_color(85.0, 0.9), ansi::YELLOW);
        // 70% projected late in window: red
        assert_eq!(pace_color(70.0, 0.8), ansi::RED);
    }
}
