//! Layout-aware segment fitting.
//!
//! Replaces the previous binary `full`/`compact` mode with a continuous
//! adaptive layout: each segment declares a priority and 1-3 variants
//! (full, compact, micro). The fitting algorithm starts with everything
//! at FULL and downgrades the lowest-priority segments first until the
//! line fits within `target_width`.
//!
//! Pipeline:
//!   1. Render produces a `SegmentBag` with all segments at their FULL form.
//!   2. `bag.fit(target_width, mode)` selects the variant per segment.
//!   3. The chosen variants are joined with a separator.
//!
//! Algorithmic complexity: O(N²) worst case where N is segment count (~15).
//! At human-readable speeds (microseconds) this is irrelevant.

use crate::ansi::visible_length;
use crate::config::{Config, Mode};

/// Drop order. Lower number = more important = drops last.
/// `Critical` segments NEVER drop — they can only downgrade variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Critical = 1,
    Important = 2,
    Normal = 3,
    Optional = 4,
}

/// One rendered segment with up to three variants. `full` is required;
/// `compact` and `micro` are optional shorter forms the fitter can fall
/// back to under width pressure.
#[derive(Debug, Clone)]
pub struct Seg {
    pub id: &'static str,
    pub priority: Priority,
    pub full: String,
    pub compact: Option<String>,
    pub micro: Option<String>,
    pub is_red: bool,
}

impl Seg {
    pub fn new(id: &'static str, priority: Priority, full: String) -> Self {
        Self { id, priority, full, compact: None, micro: None, is_red: false }
    }

    pub fn with_compact(mut self, compact: String) -> Self {
        self.compact = Some(compact);
        self
    }

    pub fn with_micro(mut self, micro: String) -> Self {
        self.micro = Some(micro);
        self
    }

    pub fn red(mut self) -> Self {
        self.is_red = true;
        self
    }
}

/// Which variant a segment is currently rendered at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    Full,
    Compact,
    Micro,
    Dropped,
}

impl Variant {
    fn next_smaller(self, seg: &Seg) -> Option<Variant> {
        match self {
            Variant::Full if seg.compact.is_some() => Some(Variant::Compact),
            Variant::Full if seg.micro.is_some() => Some(Variant::Micro),
            Variant::Compact if seg.micro.is_some() => Some(Variant::Micro),
            // Non-Critical segments can drop entirely as a last resort.
            Variant::Full | Variant::Compact | Variant::Micro => Some(Variant::Dropped),
            Variant::Dropped => None,
        }
    }
}

fn render_with(seg: &Seg, v: Variant) -> Option<&str> {
    match v {
        Variant::Full => Some(seg.full.as_str()),
        Variant::Compact => seg.compact.as_deref(),
        Variant::Micro => seg.micro.as_deref(),
        Variant::Dropped => None,
    }
}

pub struct SegmentBag<'a> {
    segs: Vec<Seg>,
    pub red_signals: u32,
    cfg: &'a Config,
}

impl<'a> SegmentBag<'a> {
    pub fn new(cfg: &'a Config) -> Self {
        Self { segs: Vec::with_capacity(16), red_signals: 0, cfg }
    }

    pub fn push(&mut self, seg: Seg) {
        if self.cfg.is_hidden(seg.id) { return; }
        if seg.is_red { self.red_signals += 1; }
        self.segs.push(seg);
    }

    /// Fit the bag to `target_width` and produce the joined line.
    /// `term_width = None` means we couldn't detect — defaults to FULL render.
    ///
    /// The returned `FitResult` is the rendered line plus metadata about
    /// which variant counts were chosen (useful for debug-timing flush).
    pub fn fit(&self, term_width: Option<u16>, sep: &str) -> FitResult {
        let sep_visible = visible_length(sep);
        // Mode::Full / Mode::Compact have NO width budget — they're "use this
        // variant for every segment, full stop" settings. Only Mode::Auto
        // actually runs the width-driven downgrade loop.
        let target = match self.cfg.mode {
            Mode::Auto => term_width,
            _ => None,
        };

        // Initial state: every segment at its preferred variant for the mode.
        let mut states: Vec<Variant> = self.segs.iter().map(|s| match self.cfg.mode {
            Mode::Compact => {
                // Compact mode: prefer micro→compact→full in that order so
                // STATUSLINE_MODE=compact remains the "shortest legible" choice.
                if s.micro.is_some() { Variant::Micro }
                else if s.compact.is_some() { Variant::Compact }
                else { Variant::Full }
            }
            _ => Variant::Full,
        }).collect();

        // For Mode::Full or when no width detected: render whatever the initial state is.
        if let Some(t) = target {
            // Greedy downgrade until it fits.
            // We work in priority-descending order (Optional first, Critical last).
            loop {
                let current_width = self.compute_width(&states, sep_visible);
                if current_width <= t as usize { break; }

                // Find a segment we can downgrade. Prefer:
                //   1. lower priority (drop optional stuff first)
                //   2. larger current variant (more savings)
                //   3. larger current rendered width (more savings)
                let candidate = self.pick_downgrade_candidate(&states);
                let Some(idx) = candidate else { break; };

                let Some(next) = states[idx].next_smaller(&self.segs[idx]) else {
                    break;
                };
                // Don't drop a Critical segment — degrade as far as it goes,
                // then stop instead of removing it.
                if matches!(next, Variant::Dropped)
                    && self.segs[idx].priority == Priority::Critical
                {
                    // Mark as un-downgradeable for subsequent iterations by
                    // setting state to Dropped... but we DON'T render it.
                    // Actually we just skip it — it stays at its current
                    // smallest variant. Use a sentinel to avoid revisiting.
                    states[idx] = match states[idx] {
                        Variant::Full | Variant::Compact => Variant::Micro,
                        other => other,
                    };
                    // If we'd already be at the smallest, break to avoid
                    // infinite loop.
                    if self.pick_downgrade_candidate(&states) == Some(idx) {
                        break;
                    }
                    continue;
                }
                states[idx] = next;
            }
        }

        // Join the surviving variants.
        let mut pieces: Vec<&str> = Vec::with_capacity(self.segs.len());
        for (i, s) in self.segs.iter().enumerate() {
            if let Some(rendered) = render_with(s, states[i]) {
                pieces.push(rendered);
            }
        }
        let joined = pieces.join(sep);

        let (full_count, compact_count, micro_count, dropped_count) = states
            .iter()
            .fold((0u32, 0u32, 0u32, 0u32), |(f, c, m, d), v| match v {
                Variant::Full => (f + 1, c, m, d),
                Variant::Compact => (f, c + 1, m, d),
                Variant::Micro => (f, c, m + 1, d),
                Variant::Dropped => (f, c, m, d + 1),
            });

        FitResult {
            line: joined,
            full_count,
            compact_count,
            micro_count,
            dropped_count,
        }
    }

    fn compute_width(&self, states: &[Variant], sep_visible: usize) -> usize {
        let mut total = 0usize;
        let mut rendered = 0usize;
        for (i, s) in self.segs.iter().enumerate() {
            if let Some(text) = render_with(s, states[i]) {
                total += visible_length(text);
                rendered += 1;
            }
        }
        if rendered > 1 { total += (rendered - 1) * sep_visible; }
        total
    }

    /// Pick the next segment to downgrade. Returns None if no segment
    /// has a smaller variant available.
    fn pick_downgrade_candidate(&self, states: &[Variant]) -> Option<usize> {
        let mut best: Option<(usize, (Priority, usize))> = None;
        for (i, s) in self.segs.iter().enumerate() {
            // Skip already-dropped segments
            if states[i] == Variant::Dropped { continue; }
            // Can we go smaller? Even a Critical segment can downgrade to
            // Micro, just not to Dropped.
            let can_downgrade = states[i].next_smaller(s).is_some()
                && !(states[i] == Variant::Micro && s.priority == Priority::Critical);
            if !can_downgrade { continue; }

            // Selection key: (priority [higher number = drops first],
            //                 current rendered width [larger = more savings])
            let cur_width = render_with(s, states[i])
                .map(visible_length)
                .unwrap_or(0);
            let key = (s.priority, cur_width);

            best = match best {
                None => Some((i, key)),
                // Prefer LOWER priority drop (i.e. Optional > Normal > Important > Critical)
                // — higher numeric value of Priority drops first.
                Some((_, best_key)) if key > best_key => Some((i, key)),
                Some(_) => best,
            };
        }
        best.map(|(i, _)| i)
    }
}

#[derive(Debug)]
pub struct FitResult {
    pub line: String,
    pub full_count: u32,
    pub compact_count: u32,
    pub micro_count: u32,
    pub dropped_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn cfg(mode: Mode) -> Config {
        Config {
            mode,
            hidden: Default::default(),
            width_override: None,
            width_margin: 0,
            debug_timing: false,
            debug_width: false,
            show_plugins: false,
        }
    }

    fn three_seg_bag<'a>(cfg: &'a Config) -> SegmentBag<'a> {
        let mut bag = SegmentBag::new(cfg);
        bag.push(Seg::new("a", Priority::Critical, "AAAA".into())
            .with_compact("AA".into())
            .with_micro("A".into()));
        bag.push(Seg::new("b", Priority::Important, "BBBBBB".into())
            .with_compact("BB".into()));
        bag.push(Seg::new("c", Priority::Optional, "CCCCCCCC".into())
            .with_compact("CC".into()));
        bag
    }

    #[test]
    fn full_mode_renders_full_variants_even_when_wide() {
        let c = cfg(Mode::Full);
        let bag = three_seg_bag(&c);
        let r = bag.fit(Some(10), " | ");
        // Mode::Full ignores the width
        assert_eq!(r.line, "AAAA | BBBBBB | CCCCCCCC");
        assert_eq!(r.full_count, 3);
    }

    #[test]
    fn compact_mode_prefers_smallest_available() {
        let c = cfg(Mode::Compact);
        let bag = three_seg_bag(&c);
        let r = bag.fit(None, " | ");
        // a has micro, b/c only compact. compact mode picks smallest available.
        assert_eq!(r.line, "A | BB | CC");
    }

    #[test]
    fn auto_drops_optional_first() {
        let c = cfg(Mode::Auto);
        let bag = three_seg_bag(&c);
        // Width forces some downgrade. c (Optional) should compact first.
        let r = bag.fit(Some(20), " | ");
        // "AAAA | BBBBBB | CC" = 18 visible — fits in 20
        assert_eq!(r.line, "AAAA | BBBBBB | CC");
    }

    #[test]
    fn auto_keeps_critical_visible_at_narrow_widths() {
        let c = cfg(Mode::Auto);
        let bag = three_seg_bag(&c);
        // Squeeze to 5 chars — only critical 'a' should survive
        let r = bag.fit(Some(5), " | ");
        // 'a' micro = "A", might be only segment left
        assert!(r.line.contains('A'));
        assert!(r.dropped_count >= 1);
    }

    #[test]
    fn auto_passes_through_when_everything_fits() {
        let c = cfg(Mode::Auto);
        let bag = three_seg_bag(&c);
        let r = bag.fit(Some(100), " | ");
        assert_eq!(r.line, "AAAA | BBBBBB | CCCCCCCC");
        assert_eq!(r.full_count, 3);
    }

    #[test]
    fn no_width_detected_renders_full() {
        let c = cfg(Mode::Auto);
        let bag = three_seg_bag(&c);
        let r = bag.fit(None, " | ");
        assert_eq!(r.line, "AAAA | BBBBBB | CCCCCCCC");
    }

    #[test]
    fn hidden_segments_dont_render() {
        let mut c = cfg(Mode::Full);
        c.hidden = ["b"].iter().map(|s| s.to_string()).collect();
        let bag = three_seg_bag(&c);
        let r = bag.fit(None, " | ");
        assert_eq!(r.line, "AAAA | CCCCCCCC");
    }
}
