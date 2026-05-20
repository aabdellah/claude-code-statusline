//! Per-segment renderers — one file per status-line block.
//!
//! Adding a new segment is three steps:
//!   1. Create `src/segments/foo.rs` containing
//!      `pub fn render(ctx: &RenderContext) -> Option<Seg>`
//!   2. Add `pub mod foo;` below
//!   3. Insert `foo::render` at the right position in `FUNCS`
//!
//! Each segment is a pure function from `&RenderContext` to `Option<Seg>`.
//! All I/O (git, transcript, anthropic status, etc.) is precomputed in
//! `RenderContext` so segments themselves never touch the filesystem or
//! spawn subprocesses. That makes them trivially testable in isolation.

pub mod anthropic;
pub mod cache;
pub mod capabilities;
pub mod context;
pub mod cost;
pub mod cwd_drift;
pub mod destruction;
pub mod duration;
pub mod model;
pub mod output_style;
pub mod perf;
pub mod rate_limits;
pub mod repo;
pub mod todo;
pub mod yak;

use crate::context::RenderContext;
use crate::layout::Seg;

/// Ordered list of segment renderers. Order matters — earlier entries
/// render to the LEFT of later ones. The fitter operates on whatever
/// each function returns.
///
/// All entries have the same signature `fn(&RenderContext) -> Option<Seg>`.
pub static FUNCS: &[fn(&RenderContext) -> Option<Seg>] = &[
    model::render,
    anthropic::render,
    repo::render,
    destruction::render,
    todo::render,
    cwd_drift::render,
    yak::render,
    context::render,
    capabilities::render,
    output_style::render,
    rate_limits::render,
    cache::render,
    cost::render,
    perf::render,
    duration::render,
];
