//! API time fraction — what percentage of the total session wall-clock was
//! spent waiting for Anthropic API responses (model thinking + streaming).
//!
//!   Low  api %  → efficient: most of the session was you-typing/thinking
//!   High api %  → lots of passive waiting on the model (long thinking,
//!                 big contexts, slow features)
//!
//! Comes from CC v2.1.132+'s `cost.total_api_duration_ms`. Hidden when
//! we don't have enough wall-clock yet for the ratio to be meaningful.

use crate::ansi::DIM;
use crate::context::RenderContext;
use crate::layout::{Priority, Seg};
use crate::repr;

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let cost = ctx.input.cost.as_ref()?;
    let total = cost.total_duration_ms?;
    let api = cost.total_api_duration_ms?;
    // Need >= 30s of wall-clock for the ratio to stabilize. Below that,
    // a single thinking turn can swing the percent wildly.
    if total < 30_000 {
        return None;
    }
    let pct = (api as f64 / total as f64 * 100.0).clamp(0.0, 100.0);

    // Informational signal — neither high nor low is inherently "bad", so
    // dim throughout rather than a threshold-based color escalation.
    let (full, compact) = repr::percent("api", "api", pct, DIM);
    Some(Seg::new("api", Priority::Normal, full).with_compact(compact))
}
