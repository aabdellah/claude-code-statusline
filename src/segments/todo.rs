//! TODO/FIXME delta — net added vs removed TODO/FIXME tokens in the
//! working-tree diff. Only renders when the tree is dirty (clean tree
//! ⇒ empty diff ⇒ zero delta ⇒ no signal worth showing).

use crate::ansi::{DIM, GREEN, RESET, YELLOW};
use crate::config;
use crate::context::RenderContext;
use crate::git;
use crate::layout::{Priority, Seg};

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    if !ctx.in_repo || !ctx.git_status.dirty {
        return None;
    }
    let delta = config::timed("todo-delta", ctx.cfg.debug_timing, || {
        git::todo_delta(ctx.cwd_path())
    });
    if delta == 0 {
        return None;
    }

    let sign = if delta > 0 {
        format!("{}+{}{}", YELLOW, delta, RESET)
    } else {
        format!("{}{}{}", GREEN, delta, RESET)
    };

    Some(
        Seg::new(
            "todo",
            Priority::Optional,
            format!("{}todo{} {}", DIM, RESET, sign),
        )
        .with_compact(format!("{}t{}{}", DIM, RESET, sign)),
    )
}
