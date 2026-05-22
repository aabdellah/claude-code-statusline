//! Cache hit % + TTL countdown.
//!   full     "cache 84% ttl 2:47"
//!   compact  "c:84 2:47"
//!
//! TTL comes from the most recent timestamped transcript entry — Anthropic's
//! prompt cache window is 5 minutes from each cache touch. When the TTL drops
//! below 60s the color goes red.

use crate::ansi::{GREEN, RED, RESET, YELLOW};
use crate::context::RenderContext;
use crate::format::fmt_ttl;
use crate::layout::{Priority, Seg};
use crate::repr;

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let usage = ctx
        .input
        .context_window
        .as_ref()
        .and_then(|cw| cw.current_usage.as_ref());
    let cache_read = usage.and_then(|u| u.cache_read_input_tokens).unwrap_or(0);
    let cache_create = usage.and_then(|u| u.cache_creation_input_tokens).unwrap_or(0);
    let cache_total = cache_read + cache_create;
    let ttl_ms = ctx.cache_ttl_ms;

    let mut full_bits: Vec<String> = Vec::new();
    let mut compact_bits: Vec<String> = Vec::new();

    if cache_total > 0 {
        let hit_pct = cache_read as f64 / cache_total as f64 * 100.0;
        let col = if hit_pct >= 80.0 {
            GREEN
        } else if hit_pct >= 50.0 {
            YELLOW
        } else {
            RED
        };
        let (full, compact) = repr::percent("cache", "c", hit_pct, col);
        full_bits.push(full);
        compact_bits.push(compact);
    }

    if let Some(ms) = ttl_ms {
        if let Some(ttl_str) = fmt_ttl(ms) {
            let ttl_color = if ms < 60_000 {
                RED
            } else if ms < 180_000 {
                YELLOW
            } else {
                GREEN
            };
            full_bits.push(format!("{}ttl {}{}", ttl_color, ttl_str, RESET));
            compact_bits.push(format!("{}{}{}", ttl_color, ttl_str, RESET));
        }
    }

    if full_bits.is_empty() {
        return None;
    }

    Some(
        Seg::new("cache", Priority::Normal, full_bits.join(" "))
            .with_compact(compact_bits.join(" ")),
    )
}
