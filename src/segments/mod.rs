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
//!
//! # Representation conventions
//!
//! For consistency across the line, use `crate::repr` helpers when the
//! segment matches one of the canonical shapes. The rules:
//!
//! | Shape         | Full mode        | Compact mode     | Helper                |
//! |---------------|------------------|------------------|-----------------------|
//! | Counter       | `label:N`        | `label_short:N`  | `repr::counter`       |
//! | Percent       | `label N%`       | `label_short:N`  | `repr::percent`       |
//! | Signed delta  | `label +N`       | `label_short:+N` | `repr::signed_delta`  |
//! | Labeled state | `label:value`    | `label_s:val_s`  | `repr::labeled_status`|
//!
//! Inline (not a repr shape):
//!   - Glyph-prefixed counts (●3, ↑5, #247) — the glyph IS the label.
//!   - Atomic values (currency `$4.21`, durations `47m`, names, IDs).
//!   - Multi-color compound assemblies (cost, capabilities).
//!
//! If your new segment fits a repr shape, USE the helper. The shape rules
//! are enforced at the type level — there's no way to accidentally produce
//! `rmN` (no separator) or `cache:84%` (wrong-mode separator) by calling
//! `repr::counter` or `repr::percent`.

pub mod anthropic;
pub mod api_time;
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
    api_time::render,
    duration::render,
];
