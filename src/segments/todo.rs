//! TODO/FIXME delta — net added vs removed TODO/FIXME tokens in the
//! working-tree diff. Only renders when the tree is dirty (clean tree
//! ⇒ empty diff ⇒ zero delta ⇒ no signal worth showing).

use crate::ansi::{DIM, GREEN, YELLOW};
use crate::context::RenderContext;
use crate::layout::{Priority, Seg};
use crate::repr;

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let delta = ctx.todo_delta;
    if delta == 0 {
        return None;
    }

    // Color the sign+number based on direction:
    //   +N (more TODOs)   → yellow (you're accumulating debt)
    //   -N (fewer TODOs)  → green  (you're paying it down)
    let sign_color = if delta > 0 { YELLOW } else { GREEN };
    let (full, compact) = repr::signed_delta("todo", "t", delta, DIM, sign_color);
    Some(Seg::new("todo", Priority::Optional, full).with_compact(compact))
}
