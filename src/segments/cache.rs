//! Prompt-cache miss alert for the latest turn.
//!   full     "cache 43%"
//!   compact  "c:43"
//!
//! hit% = cache_read / (input + cache_creation + cache_read), summed over
//! EVERY API call of the latest turn (transcript-derived, deduped per
//! message id — see `transcript::last_turn_cache_usage`).
//!
//! Alert-only: hidden while the turn's hit ratio is ≥ `SHOW_BELOW_PCT`.
//! CC renders the statusline at turn boundaries, so the *latest call* of a
//! turn is nearly always warm (earlier calls in the same turn rebuilt the
//! prefix) — a per-call number sat at a green 90%+ forever and hid the
//! very misses worth knowing about: the first call after a >5min idle gap,
//! after compaction, after a model switch or a CLAUDE.md / MCP change.
//! Aggregating over the turn keeps those misses visible; alert-only
//! keeps the line clear the rest of the time.
//!
//! The denominator is every input token, not just the cached ones:
//! `input_tokens` are uncached misses billed at the full rate,
//! `cache_creation_input_tokens` are misses billed at the write premium,
//! `cache_read_input_tokens` are the hits billed at the ~10% read rate.
//!
//! Falls back to stdin's single-call `context_window.current_usage` when
//! no transcript is available (same formula, same threshold).

use crate::ansi::{RED, YELLOW};
use crate::context::RenderContext;
use crate::layout::{Priority, Seg};
use crate::repr;

/// Render only when the turn's hit ratio is below this.
pub const SHOW_BELOW_PCT: f64 = 80.0;
/// Below this the alert turns red (and counts as a red signal).
pub const RED_BELOW_PCT: f64 = 50.0;

/// Hit ratio in percent, or `None` when there were no input tokens at all.
pub fn hit_pct(input: u64, cache_create: u64, cache_read: u64) -> Option<f64> {
    let total = input.saturating_add(cache_create).saturating_add(cache_read);
    if total == 0 {
        return None;
    }
    Some(cache_read as f64 / total as f64 * 100.0)
}

/// Turn-aggregate ratio from the transcript, else the single-call ratio
/// from stdin.
fn turn_hit_pct(ctx: &RenderContext) -> Option<f64> {
    if let Some(t) = ctx.turn_cache {
        return hit_pct(t.input, t.create, t.read);
    }
    let u = ctx
        .input
        .context_window
        .as_ref()
        .and_then(|cw| cw.current_usage.as_ref())?;
    hit_pct(
        u.input_tokens.unwrap_or(0),
        u.cache_creation_input_tokens.unwrap_or(0),
        u.cache_read_input_tokens.unwrap_or(0),
    )
}

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let pct = turn_hit_pct(ctx)?;
    if pct >= SHOW_BELOW_PCT {
        return None;
    }
    let is_red = pct < RED_BELOW_PCT;
    let col = if is_red { RED } else { YELLOW };
    let (full, compact) = repr::percent("cache", "c", pct, col);
    let mut seg = Seg::new("cache", Priority::Normal, full).with_compact(compact);
    if is_red {
        seg = seg.red();
    }
    Some(seg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::input::StatusInput;
    use crate::transcript::TurnCacheUsage;

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
    fn warm_turn_renders_nothing() {
        let input = StatusInput::default();
        let cfg = Config::from_env();
        let mut ctx = RenderContext::test_default(&input, &cfg);
        ctx.turn_cache = Some(TurnCacheUsage { input: 100, create: 2_000, read: 90_000 });
        assert!(render(&ctx).is_none());
    }

    #[test]
    fn cold_turn_renders_yellow_then_red() {
        let input = StatusInput::default();
        let cfg = Config::from_env();
        let mut ctx = RenderContext::test_default(&input, &cfg);
        ctx.turn_cache = Some(TurnCacheUsage { input: 30_000, create: 0, read: 70_000 });
        let seg = render(&ctx).expect("70% should render");
        assert_eq!(seg.red_count, 0);

        ctx.turn_cache = Some(TurnCacheUsage { input: 60_000, create: 0, read: 40_000 });
        let seg = render(&ctx).expect("40% should render");
        assert_eq!(seg.red_count, 1);
    }
}
