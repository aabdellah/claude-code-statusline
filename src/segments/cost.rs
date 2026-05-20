//! Cost segment — the money trail.
//!   full     "$4.21 $5.37/h +247/-89 $0.017/LOC lpm 14"
//!   compact  "$4.21 $5.4/h +247/-89"
//!   micro    "$4.21"
//!
//! `lpm N` = lines accepted per minute of API time (replaces the old `mpt`
//! which broke when CC v2.1.132+ changed the token-counter semantics).
//!
//! Important priority — survives most width pressure but can micro-down
//! to just the total cost as a last resort.

use crate::ansi::{DIM, GREEN, RED, RESET};
use crate::context::RenderContext;
use crate::format::{
    fmt_burn_rate, fmt_burn_rate_compact, fmt_dollars_per_loc, fmt_lines_compact,
    fmt_lines_per_api_min, fmt_money, fmt_money_compact,
};
use crate::layout::{Priority, Seg};

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let cost = ctx.input.cost.as_ref();
    let usd_opt = cost.and_then(|c| c.total_cost_usd);
    let dur_opt = cost.and_then(|c| c.total_duration_ms);
    let api_dur = cost.and_then(|c| c.total_api_duration_ms).unwrap_or(0);
    let added = cost.and_then(|c| c.total_lines_added).unwrap_or(0);
    let removed = cost.and_then(|c| c.total_lines_removed).unwrap_or(0);

    let money = usd_opt.and_then(fmt_money);
    let money_c = usd_opt.and_then(fmt_money_compact);
    let burn = match (usd_opt, dur_opt) {
        (Some(u), Some(d)) => fmt_burn_rate(u, d),
        _ => None,
    };
    let burn_c = match (usd_opt, dur_opt) {
        (Some(u), Some(d)) => fmt_burn_rate_compact(u, d),
        _ => None,
    };
    let per_loc = usd_opt.and_then(|u| fmt_dollars_per_loc(u, added));
    // Productivity now keyed off API time (cumulative model-active wall-clock)
    // since per-token cumulative totals are no longer exposed in v2.1.132+.
    let mileage = fmt_lines_per_api_min(added, api_dur);

    let mut full_bits: Vec<String> = Vec::new();
    let mut compact_bits: Vec<String> = Vec::new();
    if let Some(m) = money.as_ref() {
        full_bits.push(format!("{}{}{}", DIM, m, RESET));
        compact_bits.push(format!("{}{}{}", DIM, money_c.as_deref().unwrap_or(m), RESET));
    }
    if let Some(b) = burn.as_ref() {
        full_bits.push(format!("{}{}{}", DIM, b, RESET));
        compact_bits.push(format!("{}{}{}", DIM, burn_c.as_deref().unwrap_or(b), RESET));
    }
    if added > 0 || removed > 0 {
        full_bits.push(format!(
            "{}+{}{}{}/{}{}-{}{}",
            GREEN, added, RESET, DIM, RESET, RED, removed, RESET
        ));
        compact_bits.push(format!(
            "{}+{}{}{}/{}{}-{}{}",
            GREEN,
            fmt_lines_compact(added),
            RESET,
            DIM,
            RESET,
            RED,
            fmt_lines_compact(removed),
            RESET
        ));
    }
    // $/LOC + mileage only in full mode — nice-to-have meta.
    if let Some(p) = per_loc {
        full_bits.push(format!("{}{}{}", DIM, p, RESET));
    }
    if let Some(m) = mileage {
        full_bits.push(format!("{}{}{}", DIM, m, RESET));
    }

    if full_bits.is_empty() {
        return None;
    }

    // Micro = just the dollar total. The one number that captures the session's cost.
    let micro = money
        .as_ref()
        .or(money_c.as_ref())
        .map(|m| format!("{}{}{}", DIM, m, RESET));

    let mut seg = Seg::new("cost", Priority::Important, full_bits.join(" "))
        .with_compact(compact_bits.join(" "));
    if let Some(m) = micro {
        seg = seg.with_micro(m);
    }
    Some(seg)
}
