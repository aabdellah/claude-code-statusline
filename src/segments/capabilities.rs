//! Effort + fast — the model-state capabilities you've set that affect
//! cost/quality.
//!   full     "medium fast"
//!   compact  "mediumF"
//!   micro    "medF"
//!
//! Contributes 1 red signal at xhigh/max effort.
//!
//! Note: we deliberately do NOT render `thinking.enabled` separately. In
//! current Claude Code, `/effort` is the user-facing control, and effort
//! level >= "high" implies thinking is on; effort < "high" implies it's
//! off. So `thinking.enabled` is effectively redundant with the effort
//! word. We keep the field parsed in `input.rs` for forward compatibility
//! (if CC ever decouples them, we can re-add). Until then, just the
//! effort word — aligned with how CC talks about it.

use crate::ansi::{BOLD, BRIGHT_MAGENTA, DIM, GREEN, RED, YELLOW};
use crate::context::RenderContext;
use crate::layout::{Priority, Seg};

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let mut full_bits: Vec<String> = Vec::new();
    let mut compact_bits: Vec<String> = Vec::new();
    let mut micro_bits: Vec<String> = Vec::new();
    let mut is_red = false;

    if let Some(lvl) = ctx.input.effort.as_ref().and_then(|e| e.level.as_deref()) {
        let col: String = match lvl {
            "max" => format!("{}{}", BOLD, RED),
            "xhigh" => RED.to_string(),
            "high" => YELLOW.to_string(),
            "medium" => GREEN.to_string(),
            _ => DIM.to_string(),
        };
        let micro_lvl: String = lvl.chars().take(3).collect();
        full_bits.push(format!("{}{}{}", col, lvl, crate::ansi::RESET));
        compact_bits.push(format!("{}{}{}", col, lvl, crate::ansi::RESET));
        micro_bits.push(format!("{}{}{}", col, micro_lvl, crate::ansi::RESET));
        if lvl == "max" || lvl == "xhigh" {
            is_red = true;
        }
    }

    if ctx.input.fast_mode.unwrap_or(false) {
        full_bits.push(format!("{}{}fast{}", BOLD, BRIGHT_MAGENTA, crate::ansi::RESET));
        compact_bits.push(format!("{}{}F{}", BOLD, BRIGHT_MAGENTA, crate::ansi::RESET));
        micro_bits.push(format!("{}{}F{}", BOLD, BRIGHT_MAGENTA, crate::ansi::RESET));
    }

    if full_bits.is_empty() {
        return None;
    }

    let mut seg = Seg::new("capabilities", Priority::Important, full_bits.join(" "))
        .with_compact(compact_bits.join(""))
        .with_micro(micro_bits.join(""));
    if is_red {
        seg = seg.red();
    }
    Some(seg)
}
