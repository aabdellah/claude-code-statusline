//! cwd-drift warning — fires when Claude Code's cwd has wandered off
//! the workspace's project_dir AND we're not in a worktree (where drift
//! is expected). A subtle "you might not be where you think" signal.

use std::path::Path;

use crate::ansi::{RESET, YELLOW};
use crate::context::RenderContext;
use crate::layout::{Priority, Seg};

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    if ctx.in_worktree {
        return None;
    }
    let project_dir = ctx.project_dir.as_deref()?;
    let pd_path = Path::new(project_dir);
    if pd_path == ctx.cwd_path() {
        return None;
    }
    if ctx.cwd_path().starts_with(pd_path) {
        return None;
    }
    Some(Seg::new(
        "cwd-drift",
        Priority::Optional,
        format!("{}cwd≠proj{}", YELLOW, RESET),
    ))
}
