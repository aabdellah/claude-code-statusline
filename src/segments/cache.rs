//! Prompt-cache hit ratio for the latest API call.
//!   full     "cache 84%"
//!   compact  "c:84"
//!
//! hit% = cache_read / (input + cache_creation + cache_read)
//!
//! The denominator is EVERY input token of the call, not just the cached
//! ones: `input_tokens` are the uncached misses billed at the full rate,
//! `cache_creation_input_tokens` are misses billed at the write premium,
//! `cache_read_input_tokens` are the hits billed at the ~10% read rate.
//! Dividing by read+create alone (the previous formula) reported 100% on a
//! call that was mostly uncached input.
//!
//! No TTL countdown: CC renders the statusline only at turn boundaries, so
//! a countdown could never tick while the user sat idle — it was a frozen
//! number by the time anyone looked at it.

use crate::ansi::{GREEN, RED, YELLOW};
use crate::context::RenderContext;
use crate::layout::{Priority, Seg};
use crate::repr;

/// Hit ratio in percent, or `None` when the call had no input tokens at all.
pub fn hit_pct(input: u64, cache_create: u64, cache_read: u64) -> Option<f64> {
    let total = input.saturating_add(cache_create).saturating_add(cache_read);
    if total == 0 {
        return None;
    }
    Some(cache_read as f64 / total as f64 * 100.0)
}

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let usage = ctx
        .input
        .context_window
        .as_ref()
        .and_then(|cw| cw.current_usage.as_ref())?;
    let input = usage.input_tokens.unwrap_or(0);
    let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
    let cache_create = usage.cache_creation_input_tokens.unwrap_or(0);
    let pct = hit_pct(input, cache_create, cache_read)?;

    let col = if pct >= 80.0 {
        GREEN
    } else if pct >= 50.0 {
        YELLOW
    } else {
        RED
    };
    let (full, compact) = repr::percent("cache", "c", pct, col);
    Some(Seg::new("cache", Priority::Normal, full).with_compact(compact))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncached_input_counts_as_misses() {
        // 50k uncached + 10k read: old formula said 100%, truth is ~16.7%.
        let pct = hit_pct(50_000, 0, 10_000).unwrap();
        assert!((pct - 16.67).abs() < 0.01, "{pct}");
    }

    #[test]
    fn cache_writes_count_as_misses() {
        assert!((hit_pct(0, 20_000, 80_000).unwrap() - 80.0).abs() < 1e-9);
    }

    #[test]
    fn no_input_tokens_hides_segment() {
        assert_eq!(hit_pct(0, 0, 0), None);
    }

    #[test]
    fn fully_cached_call_is_100() {
        assert!((hit_pct(0, 0, 1).unwrap() - 100.0).abs() < 1e-9);
    }
}
