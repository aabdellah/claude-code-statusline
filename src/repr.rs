//! Canonical representation patterns for status-line segments.
//!
//! Every segment that has the shape "label + value" should produce its
//! rendered strings through one of these helpers. That enforces consistency
//! across the line:
//!
//!   counter         "label:N"           full  &  "label_short:N"   compact
//!   percent         "label N%"          full  &  "label_short:N"   compact
//!   signed_delta    "label +N" / "-N"   full  &  "label_short:+N"  compact
//!   labeled_status  "label:value"       full  &  "label_short:val" compact
//!
//! What does NOT go through repr (and shouldn't):
//!   • Glyph-prefixed values (●3, ↑5, #247) — the glyph IS the label.
//!     These stay inline in their containing segment.
//!   • Atomic values (currency, durations, model names, repo/branch) — they
//!     are self-describing; adding a label would be noise.
//!   • Multi-color compound segments (cost, capabilities) — the helpers
//!     model single-purpose values, not assemblies.
//!
//! Each helper returns `(full, compact)`. Segments stash these into a Seg
//! via `Seg::new(...).with_compact(...)`.

use crate::ansi::RESET;

/// Counter: a non-negative integer count.
///
/// Examples: `yak:1`, `wt:5`, `rm:3`.
/// Both variants use the colon separator — counts are dense by nature and
/// colon is denser than space.
pub fn counter(label_full: &str, label_compact: &str, n: u32, color: &str) -> (String, String) {
    let full = format!("{}{}:{}{}", color, label_full, n, RESET);
    let compact = format!("{}{}:{}{}", color, label_compact, n, RESET);
    (full, compact)
}

/// Percentage: a value formatted as "N%".
///
/// Examples: `ctx 78%` / `ctx:78` ; `cache 84%` / `c:84` ; `5h 64%` / `5h:64`.
/// Full uses SPACE (readable, allows the %-suffix to breathe).
/// Compact uses COLON and drops the % (denser, percentage is implied).
pub fn percent(label_full: &str, label_compact: &str, pct: f64, color: &str) -> (String, String) {
    let n = pct.round() as i64;
    let full = format!("{}{} {}%{}", color, label_full, n, RESET);
    let compact = format!("{}{}:{}{}", color, label_compact, n, RESET);
    (full, compact)
}

/// Signed delta: a positive- or negative-signed integer indicating net change.
///
/// Examples: `todo +4` / `t:+4` ; `commits -3` / `c:-3`.
///   - Label is rendered in `color_label`.
///   - Sign + number are rendered in `color_value` (typically green for net
///     negative-good, yellow for net positive-pending).
/// Full uses SPACE; compact uses COLON.
pub fn signed_delta(
    label_full: &str,
    label_compact: &str,
    n: i32,
    color_label: &str,
    color_value: &str,
) -> (String, String) {
    let sign = if n > 0 { "+" } else { "" }; // negative n already prints with `-`
    let full = format!(
        "{}{}{} {}{}{}{}",
        color_label, label_full, RESET, color_value, sign, n, RESET
    );
    let compact = format!(
        "{}{}{}:{}{}{}{}",
        color_label, label_compact, RESET, color_value, sign, n, RESET
    );
    (full, compact)
}

/// Labeled discrete-state status: a non-numeric value paired with a label.
///
/// Examples: `anthropic:critical` / `anth:cri` ; (future: `tier:standard`).
/// Both variants use the colon separator — value is a discrete state, not a
/// quantity, so the colon's "is" semantic reads correctly.
pub fn labeled_status(
    label_full: &str,
    label_compact: &str,
    value_full: &str,
    value_compact: &str,
    color: &str,
) -> (String, String) {
    let full = format!("{}{}:{}{}", color, label_full, value_full, RESET);
    let compact = format!("{}{}:{}{}", color, label_compact, value_compact, RESET);
    (full, compact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::{DIM, GREEN, YELLOW};

    /// Strip ANSI CSI escapes — assertions on visible content are clearer
    /// than scanning byte streams that have color codes interleaved.
    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut state = 0u8;
        for b in s.bytes() {
            match state {
                0 if b == 0x1b => state = 1,
                0 => out.push(b as char),
                1 => state = if b == b'[' { 2 } else { 0 },
                2 if (0x40..=0x7E).contains(&b) => state = 0,
                _ => {}
            }
        }
        out
    }

    #[test]
    fn counter_uses_colon_in_both_variants() {
        let (full, compact) = counter("yak", "y", 3, DIM);
        assert_eq!(strip(&full), "yak:3");
        assert_eq!(strip(&compact), "y:3");
    }

    #[test]
    fn percent_uses_space_in_full_and_colon_in_compact() {
        let (full, compact) = percent("cache", "c", 84.0, GREEN);
        assert_eq!(strip(&full), "cache 84%");
        assert_eq!(strip(&compact), "c:84");
    }

    #[test]
    fn percent_rounds_half_away() {
        let (_, compact) = percent("x", "x", 99.6, "");
        assert_eq!(strip(&compact), "x:100");
    }

    #[test]
    fn signed_delta_positive_prefixes_plus() {
        let (full, compact) = signed_delta("todo", "t", 4, DIM, YELLOW);
        assert_eq!(strip(&full), "todo +4");
        assert_eq!(strip(&compact), "t:+4");
    }

    #[test]
    fn signed_delta_negative_has_implicit_minus() {
        let (full, compact) = signed_delta("todo", "t", -2, DIM, GREEN);
        assert_eq!(strip(&full), "todo -2");
        assert_eq!(strip(&compact), "t:-2");
    }

    #[test]
    fn signed_delta_zero_has_no_explicit_sign() {
        // Edge case — in practice callers gate on n != 0.
        let (full, _) = signed_delta("x", "x", 0, "", "");
        assert_eq!(strip(&full), "x 0");
    }

    #[test]
    fn labeled_status_uses_colon_in_both() {
        let (full, compact) = labeled_status("anthropic", "anth", "critical", "cri", DIM);
        assert_eq!(strip(&full), "anthropic:critical");
        assert_eq!(strip(&compact), "anth:cri");
    }
}
