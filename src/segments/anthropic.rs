//! Anthropic status segment — only renders when status.claude.com reports
//! a degradation (minor / major / critical). Cached 5min, refreshed
//! lazily in the background by `anthropic::anthropic_status`.

use crate::anthropic;
use crate::ansi::{BOLD, RED, RESET, YELLOW};
use crate::config;
use crate::context::RenderContext;
use crate::layout::{Priority, Seg};

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let status = config::timed("anthropic-status", ctx.cfg.debug_timing, anthropic::anthropic_status)?;

    let col = match status.as_str() {
        "critical" => format!("{}{}", BOLD, RED),
        "major" => RED.to_string(),
        _ => YELLOW.to_string(),
    };
    let is_red = status != "minor";

    // Compact: "anth:min" / "anth:maj" / "anth:cri".
    // Char-boundary safe — never byte-slice; the indicator is unsanitized
    // upstream input and could be non-ASCII in the future.
    let short: String = status.chars().take(3).collect();

    let mut seg = Seg::new(
        "anthropic",
        Priority::Important,
        format!("{}anthropic:{}{}", col, status, RESET),
    )
    .with_compact(format!("{}anth:{}{}", col, short, RESET));

    if is_red {
        seg = seg.red();
    }
    Some(seg)
}
