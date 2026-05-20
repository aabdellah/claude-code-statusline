//! Destruction counter — number of Bash invocations this session that
//! match the "blowing things up" regex (rm/unlink/truncate/DROP/--force/--hard).
//! Only renders inside a repo and only when count > 0.

use crate::ansi::{BOLD, RED, RESET, YELLOW};
use crate::config;
use crate::context::RenderContext;
use crate::layout::{Priority, Seg};
use crate::transcript;

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    if !ctx.in_repo {
        return None;
    }
    let count = config::timed("destruction", ctx.cfg.debug_timing, || {
        transcript::destruction_count(&ctx.transcript)
    });
    if count == 0 {
        return None;
    }

    let col: String = if count >= 6 {
        format!("{}{}", BOLD, RED)
    } else if count >= 3 {
        RED.to_string()
    } else {
        YELLOW.to_string()
    };

    let mut seg = Seg::new(
        "destruction",
        Priority::Optional,
        format!("{}rm:{}{}", col, count, RESET),
    )
    .with_compact(format!("{}rm{}{}", col, count, RESET));

    if count >= 3 {
        seg = seg.red();
    }
    Some(seg)
}
