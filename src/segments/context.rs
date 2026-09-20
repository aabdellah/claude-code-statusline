//! Context meter — "ctx 78% ████████░░ 167k" with truecolor gradient bar.
//! Critical priority: never drops. Can downgrade to "78%/167k" (compact) or
//! "78%" (micro) at narrow widths.
//!
//! The percentage is distance to AUTO-COMPACTION, not fill of the model
//! window. CC's `used_percentage` divides by `context_window_size` (the
//! model window: 200k / 1M), but CC compacts at
//! `effective_window - 20k output reserve - 13k buffer`, where the
//! effective window is `CLAUDE_CODE_AUTO_COMPACT_WINDOW` if set. With a
//! 200k window and a 150k autocompact window, CC's own number reads 58%
//! at the exact moment it compacts. So: 100% here == compaction, and the
//! size suffix is the trigger the percentage is relative to. When
//! autocompact is disabled the model window is the only limit, and the
//! meter falls back to CC's raw percentage + window size.
//!
//! Contributes up to 2 red signals: one for >85% usage, one for the
//! 200k overflow marker.

use crate::ansi::{self, BOLD, DIM, RED, RESET};
use crate::context::RenderContext;
use crate::format::{compact_context_str, fmt_ctx_size};
use crate::layout::{Priority, Seg};

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let cw = ctx.input.context_window.as_ref()?;
    let raw_pct = cw
        .used_percentage
        .or_else(|| cw.remaining_percentage.map(|r| 100.0 - r))?;
    let window = cw.context_window_size.unwrap_or(cw.total_tokens.unwrap_or(0));

    // Tokens in the live window: CC's `total_input_tokens` is exactly the
    // sum CC divides for `used_percentage` (input + cache_creation +
    // cache_read). Older payloads lack it — reverse the percentage.
    let tokens = cw
        .total_input_tokens
        .filter(|&n| n > 0)
        .map(|n| n as f64)
        .unwrap_or(raw_pct / 100.0 * window as f64);

    let (used_pct, size) = match ctx.cfg.autocompact.trigger(window) {
        Some(trigger) => ((tokens / trigger as f64 * 100.0).clamp(0.0, 100.0), trigger),
        None => (raw_pct, window),
    };

    let t = (used_pct / 100.0).clamp(0.0, 1.0) as f32;
    let bar = ansi::gradient_bar(used_pct, 10, ctx.cfg.no_blink);
    let size_str = fmt_ctx_size(size);
    // `exceeds_200k_tokens` lives at the TOP LEVEL of CC's JSON, not under
    // context_window. Reading from the wrong nesting level used to make
    // this warning silently never fire.
    let exceeds = ctx.input.exceeds_200k_tokens.unwrap_or(false);

    // Full: "ctx 78% [bar] 1m"  + maybe "200k+"
    let head = format!("ctx {}%", used_pct.round() as i64);
    let mut full = format!("{} {}", ansi::grad_text(&head, t), bar);
    if !size_str.is_empty() {
        full.push_str(&format!(" {}{}{}", DIM, size_str, RESET));
    }
    if exceeds {
        full.push_str(&format!(" {}{}200k+{}", RED, BOLD, RESET));
    }

    // Compact: "78%/1m" (+ "+" if exceeds — handled by compact_context_str)
    let compact = ansi::grad_text(&compact_context_str(used_pct, size, exceeds), t);
    // Micro: just "78%"
    let micro = ansi::grad_text(&format!("{}%", used_pct.round() as i64), t);

    let mut red_count = 0u32;
    if used_pct >= 85.0 {
        red_count += 1;
    }
    if exceeds {
        red_count += 1;
    }

    let seg = Seg::new("context", Priority::Critical, full)
        .with_compact(compact)
        .with_micro(micro)
        .red_n(red_count);
    Some(seg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::strip_ansi;
    use crate::config::Config;
    use crate::context::RenderContext;
    use crate::input::{ContextWindow, StatusInput};

    fn ctx_with_window<'a>(
        input: &'a StatusInput,
        cfg: &'a Config,
    ) -> RenderContext<'a> {
        RenderContext::test_default(input, cfg)
    }

    fn input_with_pct(used_pct: f64, size: u64, exceeds: bool) -> StatusInput {
        StatusInput {
            context_window: Some(ContextWindow {
                used_percentage: Some(used_pct),
                context_window_size: Some(size),
                ..ContextWindow::default()
            }),
            exceeds_200k_tokens: Some(exceeds),
            ..StatusInput::default()
        }
    }

    #[test]
    fn hidden_when_context_window_missing() {
        let input = StatusInput::default();
        let cfg = cfg_with_autocompact(None, true);
        assert!(render(&ctx_with_window(&input, &cfg)).is_none());
    }

    #[test]
    fn below_85pct_no_red_signal() {
        let input = input_with_pct(50.0, 1_000_000, false);
        let cfg = cfg_with_autocompact(None, true);
        let seg = render(&ctx_with_window(&input, &cfg)).expect("renders");
        assert_eq!(seg.red_count, 0);
    }

    #[test]
    fn at_or_above_85pct_contributes_one_red_signal() {
        let input = input_with_pct(85.0, 1_000_000, false);
        let cfg = cfg_with_autocompact(None, true);
        let seg = render(&ctx_with_window(&input, &cfg)).expect("renders");
        assert_eq!(seg.red_count, 1, "85% threshold inclusive");
    }

    #[test]
    fn exceeds_200k_contributes_red_signal_independently() {
        // Below 85% but exceeds — single red signal (200k only).
        let input = input_with_pct(60.0, 1_000_000, true);
        let cfg = cfg_with_autocompact(None, true);
        let seg = render(&ctx_with_window(&input, &cfg)).expect("renders");
        assert_eq!(seg.red_count, 1);
    }

    #[test]
    fn both_85pct_and_200k_stack_to_two_red_signals() {
        let input = input_with_pct(90.0, 1_000_000, true);
        let cfg = cfg_with_autocompact(None, true);
        let seg = render(&ctx_with_window(&input, &cfg)).expect("renders");
        assert_eq!(seg.red_count, 2, "85% AND 200k stack independently");
    }

    #[test]
    fn full_variant_includes_200k_marker_when_exceeded() {
        let input = input_with_pct(50.0, 1_000_000, true);
        let cfg = cfg_with_autocompact(None, true);
        let seg = render(&ctx_with_window(&input, &cfg)).expect("renders");
        assert!(strip_ansi(&seg.full).contains("200k+"));
    }

    // ─── Compaction-relative percentage ──────────────────────────────────

    fn cfg_with_autocompact(window: Option<u64>, disabled: bool) -> Config {
        Config {
            autocompact: crate::config::AutoCompact { disabled, window, max_output_tokens: None },
            ..Config::from_env()
        }
    }

    fn input_with_tokens(tokens: u64, size: u64) -> StatusInput {
        StatusInput {
            context_window: Some(ContextWindow {
                used_percentage: Some((tokens as f64 / size as f64 * 100.0).round()),
                context_window_size: Some(size),
                total_input_tokens: Some(tokens),
                ..ContextWindow::default()
            }),
            ..StatusInput::default()
        }
    }

    #[test]
    fn percent_is_relative_to_compaction_trigger_not_model_window() {
        // Real numbers from a session: CC reports a 200k window, but the
        // settings env sets CLAUDE_CODE_AUTO_COMPACT_WINDOW=150000. CC
        // compacts at 150k - 20k output reserve - 13k buffer = 117k.
        // 116,844 tokens is the observed pre-compaction count: the meter
        // must read full, not 58%.
        let input = input_with_tokens(116_844, 200_000);
        let cfg = cfg_with_autocompact(Some(150_000), false);
        let seg = render(&ctx_with_window(&input, &cfg)).expect("renders");
        let full = strip_ansi(&seg.full);
        assert!(full.starts_with("ctx 100%"), "got {full}");
        assert!(full.ends_with("117k"), "suffix is the trigger, got {full}");
        assert_eq!(strip_ansi(seg.micro.as_deref().unwrap()), "100%");
        assert_eq!(seg.red_count, 1);
    }

    #[test]
    fn default_trigger_is_window_minus_reserves() {
        // No autocompact window override: trigger = 200k - 20k - 13k = 167k.
        let input = input_with_tokens(100_000, 200_000);
        let cfg = cfg_with_autocompact(None, false);
        let seg = render(&ctx_with_window(&input, &cfg)).expect("renders");
        let full = strip_ansi(&seg.full);
        assert!(full.starts_with("ctx 60%"), "100k/167k, got {full}");
        assert!(full.ends_with("167k"), "got {full}");
    }

    #[test]
    fn autocompact_disabled_falls_back_to_model_window() {
        let input = input_with_tokens(100_000, 200_000);
        let cfg = cfg_with_autocompact(None, true);
        let seg = render(&ctx_with_window(&input, &cfg)).expect("renders");
        let full = strip_ansi(&seg.full);
        assert!(full.starts_with("ctx 50%"), "got {full}");
        assert!(full.ends_with("200k"), "got {full}");
    }

    #[test]
    fn percentage_only_input_derives_tokens_from_window() {
        // Older payloads without total_input_tokens: 50% of 200k = 100k,
        // which is 60% of the 167k trigger.
        let input = input_with_pct(50.0, 200_000, false);
        let cfg = cfg_with_autocompact(None, false);
        let seg = render(&ctx_with_window(&input, &cfg)).expect("renders");
        assert!(strip_ansi(&seg.full).starts_with("ctx 60%"));
    }

    #[test]
    fn micro_variant_is_just_the_percentage() {
        let input = input_with_pct(63.0, 1_000_000, false);
        let cfg = cfg_with_autocompact(None, true);
        let seg = render(&ctx_with_window(&input, &cfg)).expect("renders");
        let micro = seg.micro.as_deref().expect("has micro");
        assert_eq!(strip_ansi(micro), "63%");
    }
}
