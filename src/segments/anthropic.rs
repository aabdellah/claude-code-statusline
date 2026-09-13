//! Claude Code status segment — only renders when status.claude.com reports
//! the *Claude Code* component as anything other than operational. Other
//! products on the page (claude.ai, Cowork, Console…) are ignored — see
//! `crate::anthropic`. Cached 5min, refreshed lazily in the background.
//!
//!   full     "claude:degraded" / "claude:partial" / "claude:outage" / "claude:maint"
//!   compact  "cc:deg" / "cc:part" / "cc:out" / "cc:mnt"

use crate::anthropic;
use crate::ansi::{BOLD, RED, YELLOW};
use crate::config;
use crate::context::RenderContext;
use crate::layout::{Priority, Seg};
use crate::repr;

/// Map a Statuspage component status to (full word, compact word, color,
/// counts-as-red). Unknown statuses render verbatim in yellow so a new
/// Statuspage vocabulary word degrades to "visible but not alarming".
fn describe(status: &str) -> (String, String, String, bool) {
    match status {
        "degraded_performance" => ("degraded".into(), "deg".into(), YELLOW.to_owned(), false),
        "under_maintenance" => ("maint".into(), "mnt".into(), YELLOW.to_owned(), false),
        "partial_outage" => ("partial".into(), "part".into(), RED.to_owned(), true),
        "major_outage" => ("outage".into(), "out".into(), format!("{}{}", BOLD, RED), true),
        other => {
            // Char-boundary safe truncation (never byte-slice; the status is
            // unsanitized upstream input and could be non-ASCII).
            let short: String = other.chars().take(3).collect();
            (other.to_owned(), short, YELLOW.to_owned(), false)
        }
    }
}

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let status = config::timed("anthropic-status", ctx.cfg.debug_timing, anthropic::anthropic_status)?;
    let (word, short, col, is_red) = describe(&status);

    let (full, compact) = repr::labeled_status("claude", "cc", &word, &short, &col);
    let mut seg = Seg::new("anthropic", Priority::Important, full).with_compact(compact);
    if is_red {
        seg = seg.red();
    }
    Some(seg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outages_are_red_degradations_are_not() {
        assert!(describe("major_outage").3);
        assert!(describe("partial_outage").3);
        assert!(!describe("degraded_performance").3);
        assert!(!describe("under_maintenance").3);
    }

    #[test]
    fn unknown_status_truncates_on_char_boundary() {
        let (full, short, _, red) = describe("étrange");
        assert_eq!(full, "étrange");
        assert_eq!(short, "étr");
        assert!(!red);
    }
}
