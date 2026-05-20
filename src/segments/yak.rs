//! Yak shave depth — nesting level of Task subagents from the transcript's
//! sourceToolAssistantUUID chain. Depth 0 = main thread (no segment);
//! 1+ = "we are N sub-problems deep from what you actually asked for."

use crate::ansi::{BOLD, DIM, GREEN, RED, RESET, YELLOW};
use crate::config;
use crate::context::RenderContext;
use crate::layout::{Priority, Seg};
use crate::transcript;

/// "yak~~~:N" — N tildes scale with depth, capped at 5.
fn yak_indicator(depth: u32) -> Option<String> {
    if depth == 0 {
        return None;
    }
    let tildes = "~".repeat((depth - 1).min(5) as usize);
    Some(format!("yak{}:{}", tildes, depth))
}

fn yak_color(depth: u32) -> String {
    match depth {
        0..=1 => DIM.to_string(),
        2 => GREEN.to_string(),
        3 => YELLOW.to_string(),
        4 => RED.to_string(),
        _ => format!("{}{}", BOLD, RED),
    }
}

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let depth = config::timed("yak-depth", ctx.cfg.debug_timing, || {
        transcript::yak_depth(&ctx.transcript)
    });
    let indicator = yak_indicator(depth)?;
    let col = yak_color(depth);

    let mut seg = Seg::new("yak", Priority::Optional, format!("{}{}{}", col, indicator, RESET))
        .with_compact(format!("{}y:{}{}", col, depth, RESET));
    if depth >= 4 {
        seg = seg.red();
    }
    Some(seg)
}
