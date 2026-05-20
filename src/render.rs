//! Render orchestration.
//!
//! Three steps:
//!   1. Build a `RenderContext` (one-shot I/O)
//!   2. Iterate `segments::FUNCS`, pushing each non-None `Seg` into the bag
//!   3. Detect terminal width, subtract host margin + CRIT prefix room,
//!      `bag.fit()` the segments to the available budget, prepend CRIT
//!      if ≥3 red signals fired.
//!
//! Layout (at full width):
//!   model · repo/branch +flags+stash ↑↓ [wt←origin] wt:N #PR · todo Δ ·
//!   ctx % [gradient bar] · ⚡effort 🧠 · ◆style · 5h N% · cache N% ttl m:ss ·
//!   $X $Y/h +A/-B · Nt/s · dur
//!
//! All per-segment logic lives in `src/segments/`. To add a new segment,
//! drop in a file with `pub fn render(ctx: &RenderContext) -> Option<Seg>`
//! and register it in `segments::mod::FUNCS`. No changes to this file needed.

use crate::ansi::{self, BOLD, DIM, RED, RESET};
use crate::config::{self, Config, Mode};
use crate::context::RenderContext;
use crate::input::StatusInput;
use crate::layout::SegmentBag;
use crate::segments;
use crate::width;

pub struct RenderOutput {
    pub line: String,
    pub term_width: Option<u16>,
    /// Variant counts: (full, compact, micro, dropped).
    pub variant_counts: (u32, u32, u32, u32),
}

pub fn render(input: &StatusInput, cfg: &Config) -> RenderOutput {
    config::reset_timings();

    let ctx = RenderContext::build(input, cfg);
    let mut bag = SegmentBag::new(cfg);
    for f in segments::FUNCS {
        if let Some(seg) = f(&ctx) {
            bag.push(seg);
        }
    }

    // --- Fit + decorate -----------------------------------------------------
    let term_width = config::timed("width-detect", cfg.debug_timing, || {
        width::detect_term_width(cfg)
    });

    // When 3+ red signals are present we prepend a CRIT banner and recolor
    // separators red. We pick the separator UP FRONT so the fitter operates
    // on the final string — no post-hoc String::replace, which would mutate
    // segment content if a segment ever embedded the separator's byte sequence.
    let crit_active = bag.red_signals >= 3;
    let sep = if crit_active {
        format!(" {}·{} ", RED, RESET)
    } else {
        format!(" {}·{} ", DIM, RESET)
    };

    // Subtract host margin (CC frame, etc.) and CRIT prefix room from the budget.
    let crit_prefix = format!("{}{}CRIT{}{}", BOLD, RED, bag.red_signals, RESET);
    let crit_prefix_visible_usize =
        ansi::visible_length(&crit_prefix) + ansi::visible_length(&sep);
    // Cap at u16::MAX rather than truncate via `as u16`. Realistic values
    // are <20, but defense against unforeseen large red_signals counts.
    let crit_prefix_visible = crit_prefix_visible_usize.min(u16::MAX as usize) as u16;
    let effective_width = term_width.map(|w| {
        let mut budget = w.saturating_sub(cfg.width_margin);
        if crit_active {
            budget = budget.saturating_sub(crit_prefix_visible);
        }
        budget
    });

    let fit = bag.fit(effective_width, &sep);

    let chosen = if crit_active {
        format!("{}{}{}", crit_prefix, sep, fit.line)
    } else {
        fit.line
    };

    RenderOutput {
        line: chosen,
        term_width,
        variant_counts: (
            fit.full_count,
            fit.compact_count,
            fit.micro_count,
            fit.dropped_count,
        ),
    }
}

/// Flush per-segment timings to stderr when `STATUSLINE_DEBUG_TIMING=1`.
pub fn flush_debug_timing(cfg: &Config, out: &RenderOutput) {
    if !cfg.debug_timing {
        return;
    }
    let timings = config::drain_timings();
    if timings.is_empty() {
        return;
    }
    let total: f64 = timings.iter().map(|(_, ms)| ms).sum();
    let width_info = out
        .term_width
        .map(|w| format!("{}cols", w))
        .unwrap_or_else(|| "width:unknown".into());
    let mode_info = match cfg.mode {
        Mode::Auto => {
            let (f, c, m, d) = out.variant_counts;
            format!("auto[full:{} cpct:{} micro:{} drop:{}]", f, c, m, d)
        }
        Mode::Full => "full".to_string(),
        Mode::Compact => "compact".to_string(),
    };
    let visible = ansi::visible_length(&out.line);
    eprintln!(
        "\n[statusline:timing] total={:.1}ms {} mode={} len={}",
        total, width_info, mode_info, visible
    );
    let mut sorted = timings;
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (name, ms) in &sorted {
        eprintln!("  {:>7.2}ms  {}", ms, name);
    }
}
