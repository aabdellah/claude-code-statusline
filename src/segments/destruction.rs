//! Destruction counter — number of Bash invocations this session that
//! match the "blowing things up" regex (rm/unlink/truncate/DROP/--force/--hard).
//! Only renders inside a repo and only when count > 0.

use crate::ansi::{BOLD, RED, YELLOW};
use crate::context::RenderContext;
use crate::layout::{Priority, Seg};
use crate::repr;

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let count = ctx.destruction_count;
    if count == 0 {
        return None;
    }

    // Color escalates with count — three discrete tiers.
    let col: String = if count >= 6 {
        format!("{}{}", BOLD, RED)
    } else if count >= 3 {
        RED.to_owned()
    } else {
        YELLOW.to_owned()
    };

    let (full, compact) = repr::counter("rm", "rm", count, &col);
    let mut seg = Seg::new("destruction", Priority::Optional, full).with_compact(compact);
    if count >= 3 {
        seg = seg.red();
    }
    Some(seg)
}
