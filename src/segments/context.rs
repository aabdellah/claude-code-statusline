//! Context meter — "ctx 78% ████████░░ 1m" with truecolor gradient bar.
//! Critical priority: never drops. Can downgrade to "78%/1m" (compact) or
//! "78%" (micro) at narrow widths.
//!
//! Contributes up to 2 red signals: one for >85% usage, one for the
//! 200k overflow marker.

use crate::ansi::{self, BOLD, DIM, RED, RESET};
use crate::context::RenderContext;
use crate::format::{compact_context_str, fmt_ctx_size};
use crate::layout::{Priority, Seg};

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let cw = ctx.input.context_window.as_ref()?;
    let used_pct = cw
        .used_percentage
        .or_else(|| cw.remaining_percentage.map(|r| 100.0 - r))?;

    let t = (used_pct / 100.0).clamp(0.0, 1.0) as f32;
    let bar = ansi::gradient_bar(used_pct, 10, ctx.cfg.no_blink);
    let size = cw.context_window_size.unwrap_or(cw.total_tokens.unwrap_or(0));
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
        let cfg = Config::from_env();
        assert!(render(&ctx_with_window(&input, &cfg)).is_none());
    }

    #[test]
    fn below_85pct_no_red_signal() {
        let input = input_with_pct(50.0, 1_000_000, false);
        let cfg = Config::from_env();
        let seg = render(&ctx_with_window(&input, &cfg)).expect("renders");
        assert_eq!(seg.red_count, 0);
    }

    #[test]
    fn at_or_above_85pct_contributes_one_red_signal() {
        let input = input_with_pct(85.0, 1_000_000, false);
        let cfg = Config::from_env();
        let seg = render(&ctx_with_window(&input, &cfg)).expect("renders");
        assert_eq!(seg.red_count, 1, "85% threshold inclusive");
    }

    #[test]
    fn exceeds_200k_contributes_red_signal_independently() {
        // Below 85% but exceeds — single red signal (200k only).
        let input = input_with_pct(60.0, 1_000_000, true);
        let cfg = Config::from_env();
        let seg = render(&ctx_with_window(&input, &cfg)).expect("renders");
        assert_eq!(seg.red_count, 1);
    }

    #[test]
    fn both_85pct_and_200k_stack_to_two_red_signals() {
        let input = input_with_pct(90.0, 1_000_000, true);
        let cfg = Config::from_env();
        let seg = render(&ctx_with_window(&input, &cfg)).expect("renders");
        assert_eq!(seg.red_count, 2, "85% AND 200k stack independently");
    }

    #[test]
    fn full_variant_includes_200k_marker_when_exceeded() {
        let input = input_with_pct(50.0, 1_000_000, true);
        let cfg = Config::from_env();
        let seg = render(&ctx_with_window(&input, &cfg)).expect("renders");
        assert!(strip_ansi(&seg.full).contains("200k+"));
    }

    #[test]
    fn micro_variant_is_just_the_percentage() {
        let input = input_with_pct(63.0, 1_000_000, false);
        let cfg = Config::from_env();
        let seg = render(&ctx_with_window(&input, &cfg)).expect("renders");
        let micro = seg.micro.as_deref().expect("has micro");
        assert_eq!(strip_ansi(micro), "63%");
    }
}
