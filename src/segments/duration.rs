//! Session duration — total wall-clock from cost.total_duration_ms.
//!   full     "47m" / "3h12m"
//!   compact  "47m" / "3h"

use crate::ansi::{DIM, RESET};
use crate::context::RenderContext;
use crate::format::{fmt_duration, fmt_duration_compact};
use crate::layout::{Priority, Seg};

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let dur_ms = ctx
        .input
        .cost
        .as_ref()
        .and_then(|c| c.total_duration_ms)
        .unwrap_or(0);
    let full = fmt_duration(dur_ms)?;
    let compact = fmt_duration_compact(dur_ms);
    Some(
        Seg::new("duration", Priority::Normal, format!("{}{}{}", DIM, full, RESET))
            .with_compact(format!(
                "{}{}{}",
                DIM,
                compact.as_deref().unwrap_or(full.as_str()),
                RESET
            )),
    )
}
