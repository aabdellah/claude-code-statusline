//! Rate limits — 5h + 7d windows with usage % and projection.
//!   full     "5h 64%→2h15m 7d 71%→98%"
//!   compact  "5h:64 7d:71/98"
//!
//! Contributes red signals: 5h ≥90%, 7d ≥90%, projection over 115% or
//! significantly underpace late in the week.

use crate::ansi::{self, DIM, RESET};
use crate::context::RenderContext;
use crate::format::fmt_reset_time;
use crate::layout::{Priority, Seg};
use crate::pace;
use crate::repr;

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let rl = ctx.input.rate_limits.as_ref()?;

    let mut full_bits: Vec<String> = Vec::new();
    let mut compact_bits: Vec<String> = Vec::new();
    let mut red_count = 0u32;

    // 5h window
    if let Some(fh) = rl.five_hour.as_ref() {
        if let Some(p) = fh.used_percentage {
            let col = ansi::pct_color(p, 70.0, 90.0);
            let (mut full, compact) = repr::percent("5h", "5h", p, col);
            let reset_str = fh.resets_at.as_ref().map(fmt_reset_time).unwrap_or_default();
            if !reset_str.is_empty() {
                full.push_str(&format!("{}→{}{}", DIM, reset_str, RESET));
            }
            full_bits.push(full);
            compact_bits.push(compact);
            if p >= 90.0 {
                red_count += 1;
            }
        }
    }

    // 7d window with pace projection
    if let Some(sd) = rl.seven_day.as_ref() {
        if let Some(pace_obj) = pace::seven_day_pace(sd) {
            let used_col = ansi::pct_color(pace_obj.used_pct, 70.0, 90.0);
            let (mut full, mut compact) = repr::percent("7d", "7d", pace_obj.used_pct, used_col);
            if let (Some(projected), Some(frac)) = (pace_obj.projected, pace_obj.frac_elapsed) {
                let pcol = pace::pace_color(projected, frac);
                // `~` prefix during the volatile early-window period
                // (frac < 0.10): the projection is real and worth showing
                // (day-1 overspend triggers red), but the exact number swings
                // hour-to-hour until ~17h into the window.
                let marker = if pace_obj.is_volatile() { "~" } else { "" };
                full.push_str(&format!(
                    " {}→{}{}%{}",
                    pcol,
                    marker,
                    projected.round() as i64,
                    RESET
                ));
                compact.push_str(&format!(
                    "{}/{}{}{}",
                    pcol,
                    marker,
                    projected.round() as i64,
                    RESET
                ));
                if projected > 115.0 || (projected < 80.0 && frac >= 0.70) {
                    red_count += 1;
                }
            }
            if pace_obj.used_pct >= 90.0 {
                red_count += 1;
            }
            full_bits.push(full);
            compact_bits.push(compact);
        }
    }

    if full_bits.is_empty() {
        return None;
    }

    Some(
        Seg::new("rate-limits", Priority::Normal, full_bits.join(" "))
            .with_compact(compact_bits.join(" "))
            .red_n(red_count),
    )
}
