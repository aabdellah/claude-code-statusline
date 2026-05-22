//! Anthropic status segment — only renders when status.claude.com reports
//! a degradation (minor / major / critical). Cached 5min, refreshed
//! lazily in the background by `anthropic::anthropic_status`.

use crate::anthropic;
use crate::ansi::{BOLD, RED, YELLOW};
use crate::config;
use crate::context::RenderContext;
use crate::layout::{Priority, Seg};
use crate::repr;

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let status = config::timed("anthropic-status", ctx.cfg.debug_timing, anthropic::anthropic_status)?;

    let col = match status.as_str() {
        "critical" => format!("{}{}", BOLD, RED),
        "major" => RED.to_owned(),
        _ => YELLOW.to_owned(),
    };
    let is_red = status != "minor";

    // Compact value: char-boundary safe truncation (never byte-slice; the
    // indicator is unsanitized upstream input and could be non-ASCII).
    let short: String = status.chars().take(3).collect();

    let (full, compact) = repr::labeled_status("anthropic", "anth", &status, &short, &col);
    let mut seg = Seg::new("anthropic", Priority::Important, full).with_compact(compact);
    if is_red {
        seg = seg.red();
    }
    Some(seg)
}
