//! Model name segment — "Opus 4.7" / "Sonnet 4.6" / "haiku-4.5".
//! Critical priority: always present, never drops.

use crate::ansi::{CYAN, RESET};
use crate::context::RenderContext;
use crate::format::{short_model_compact, short_model_name};
use crate::layout::{Priority, Seg};

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let full = format!(
        "{}{}{}",
        CYAN,
        short_model_name(ctx.input.model.as_ref()),
        RESET
    );
    let compact = format!(
        "{}{}{}",
        CYAN,
        short_model_compact(ctx.input.model.as_ref()),
        RESET
    );
    Some(
        Seg::new("model", Priority::Critical, full).with_compact(compact),
    )
}
