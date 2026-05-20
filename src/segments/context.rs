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
    let exceeds = cw.exceeds_200k_tokens.unwrap_or(false);

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
