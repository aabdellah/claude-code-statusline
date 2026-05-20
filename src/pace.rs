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

#[derive(Debug, Clone, Copy)]
pub struct Pace {
    pub used_pct: f64,
    /// `None` when we haven't elapsed enough of the window for projection to
    /// be meaningful (≥10% required, ≤100% required).
    pub projected: Option<f64>,
    pub frac_elapsed: Option<f64>,
}

pub fn seven_day_pace(seven_day: &RateLimitWindow) -> Option<Pace> {
    let used_pct = seven_day.used_percentage?;

    let mut pace = Pace { used_pct, projected: None, frac_elapsed: None };

    let Some(resets_at) = &seven_day.resets_at else { return Some(pace); };
    let reset_ms = match resets_at {
        serde_json::Value::Number(n) => {
            let v = n.as_f64()?;
            if v < 1e12 { (v * 1000.0) as i64 } else { v as i64 }
        }
        serde_json::Value::String(s) => crate::format::parse_rfc3339_ms(s)?,
        _ => return Some(pace),
    };

    let window_start = reset_ms - SEVEN_DAY_MS;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    let frac = (now_ms - window_start) as f64 / SEVEN_DAY_MS as f64;
    // <10% elapsed (~17h in): projection too volatile. >100%: window passed.
    if !(0.10..=1.0).contains(&frac) {
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
    fn pace_color_underpace_escalates_with_time() {
        // 85% projected: dim early, yellow late
        assert_eq!(pace_color(85.0, 0.5), ansi::DIM);
        assert_eq!(pace_color(85.0, 0.9), ansi::YELLOW);
        // 70% projected late in window: red
        assert_eq!(pace_color(70.0, 0.8), ansi::RED);
    }
}
