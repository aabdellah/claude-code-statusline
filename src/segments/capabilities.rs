//! Effort + thinking + fast — the model-state capabilities you've set
//! that affect cost/quality. Examples:
//!   full     "medium thinking fast"
//!   compact  "mediumTF"
//!   micro    "medTF" (3-char effort + 1-char thinking + 1-char fast)
//!
//! Contributes 1 red signal at xhigh/max effort.

use crate::ansi::{BOLD, BRIGHT_MAGENTA, DIM, GREEN, ITALIC, RED, RESET, VIOLET, YELLOW};
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
        full_bits.push(format!("{}{}{}", col, lvl, RESET));
        compact_bits.push(format!("{}{}{}", col, lvl, RESET));
        micro_bits.push(format!("{}{}{}", col, micro_lvl, RESET));
        if lvl == "max" || lvl == "xhigh" {
            is_red = true;
        }
    }
    if ctx.input.thinking.as_ref().and_then(|t| t.enabled).unwrap_or(false) {
        full_bits.push(format!("{}{}thinking{}", ITALIC, VIOLET, RESET));
        compact_bits.push(format!("{}{}T{}", ITALIC, VIOLET, RESET));
        micro_bits.push(format!("{}{}T{}", ITALIC, VIOLET, RESET));
    }
    if ctx.input.fast_mode.unwrap_or(false) {
        full_bits.push(format!("{}{}fast{}", BOLD, BRIGHT_MAGENTA, RESET));
        compact_bits.push(format!("{}{}F{}", BOLD, BRIGHT_MAGENTA, RESET));
        micro_bits.push(format!("{}{}F{}", BOLD, BRIGHT_MAGENTA, RESET));
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
