//! Yak shave depth — nesting level of Task subagents at the LATEST
//! transcript entry. Depth 0 = main thread (no segment); 1+ = "we are N
//! sub-problems deep from what you actually asked for."

use crate::ansi::{BOLD, DIM, GREEN, RED, YELLOW};
use crate::context::RenderContext;
use crate::layout::{Priority, Seg};
use crate::repr;

/// Tilde-pad the label so it grows visually shaggier with depth, capped at 5.
fn yak_label(depth: u32) -> String {
    let tildes = "~".repeat((depth - 1).min(5) as usize);
    format!("yak{}", tildes)
}

fn yak_color(depth: u32) -> String {
    match depth {
        0..=1 => DIM.to_owned(),
        2 => GREEN.to_owned(),
        3 => YELLOW.to_owned(),
        4 => RED.to_owned(),
        _ => format!("{}{}", BOLD, RED),
    }
}

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let depth = ctx.yak_depth;
    if depth == 0 {
        return None;
    }
    let col = yak_color(depth);

    // counter: "yak~~~:3" full / "y:3" compact — the tilde-padding lives in
    // the full label only; compact stays single-char for density.
    let (full, compact) = repr::counter(&yak_label(depth), "y", depth, &col);
    let mut seg = Seg::new("yak", Priority::Optional, full).with_compact(compact);
    if depth >= 4 {
        seg = seg.red();
    }
    Some(seg)
}
