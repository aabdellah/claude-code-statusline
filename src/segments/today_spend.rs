//! Today's $ + tokens across ALL sessions (cross-project rollup).
//!
//! Distinct from the per-session `cost` segment — this answers "how much
//! compute have I burned today" across every CC session that touched a
//! transcript since today's local midnight. For subscribers the $ is
//! notional (what API users would have paid) and the tokens are ground-truth.
//!
//! Hidden until the background rollup writes its first cache file (typically
//! a few seconds after the first render of a session). After that, refreshes
//! happen every ≥60s lazily in a detached background process.
//!
//!   full     "today $12.40 1.2m"
//!   compact  "td $12 1.2m"
//!   micro    "td $12"
//!
//! Sub-elements join with a single space — matching `cost.rs`'s convention so
//! the segment-level `·` separator stays unambiguous. The `$` and `m` suffixes
//! make money-vs-tokens readable without an internal delimiter.
//!
//! Priority is `Normal` — drops before the current-session `cost` segment
//! (Important), since "right-now spend" is more actionable than a rollup.

use crate::ansi::{DIM, RESET};
use crate::context::RenderContext;
use crate::format::{fmt_ctx_size, fmt_money, fmt_money_compact};
use crate::layout::{Priority, Seg};

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let today = ctx.today.as_ref()?;
    if today.cost_usd <= 0.0 && today.total_tokens == 0 {
        return None;
    }

    // Gate on >= $0.01 — fmt_money returns "<$0.01" for any tiny positive,
    // which is wrong for our "no meaningful cost yet" case (e.g. all-unknown
    // models, or a brand-new day with only a handful of tokens).
    let money_full = if today.cost_usd >= 0.01 { fmt_money(today.cost_usd) } else { None };
    let money_compact = if today.cost_usd >= 0.01 { fmt_money_compact(today.cost_usd) } else { None };
    let tokens = if today.total_tokens > 0 {
        Some(fmt_ctx_size(today.total_tokens))
    } else {
        None
    };

    // --- Full: "today $12.40 1.2m"
    let mut full_parts: Vec<String> = vec![format!("{}today{}", DIM, RESET)];
    if let Some(m) = &money_full {
        full_parts.push(format!("{}{}{}", DIM, m, RESET));
    }
    if let Some(t) = &tokens {
        full_parts.push(format!("{}{}{}", DIM, t, RESET));
    }
    let full = full_parts.join(" ");

    // --- Compact: "td $12 1.2m"
    let mut compact_parts: Vec<String> = vec![format!("{}td{}", DIM, RESET)];
    if let Some(m) = money_compact.as_ref().or(money_full.as_ref()) {
        compact_parts.push(format!("{}{}{}", DIM, m, RESET));
    }
    if let Some(t) = &tokens {
        compact_parts.push(format!("{}{}{}", DIM, t, RESET));
    }
    let compact = compact_parts.join(" ");

    // --- Micro: "td $12" (tokens dropped to save width)
    let micro = money_compact
        .as_ref()
        .or(money_full.as_ref())
        .map(|m| format!("{}td {}{}", DIM, m, RESET))
        .or_else(|| tokens.as_ref().map(|t| format!("{}td {}{}", DIM, t, RESET)))?;

    let seg = Seg::new("today", Priority::Normal, full)
        .with_compact(compact)
        .with_micro(micro);
    Some(seg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::TodayRollup;
    use crate::config::Config;
    use crate::context::RenderContext;
    use crate::git::{GitStatus, WorktreeStats};
    use crate::input::StatusInput;
    use std::path::PathBuf;

    use crate::ansi::strip_ansi as strip;

    fn ctx_with_today<'a>(
        input: &'a StatusInput,
        cfg: &'a Config,
        today: Option<TodayRollup>,
    ) -> RenderContext<'a> {
        let mut ctx = RenderContext::test_default(input, cfg);
        ctx.today = today;
        ctx
    }

    #[test]
    fn hides_when_today_missing() {
        let input = StatusInput::default();
        let cfg = Config::from_env();
        let ctx = ctx_with_today(&input, &cfg, None);
        assert!(render(&ctx).is_none());
    }

    #[test]
    fn hides_when_today_empty() {
        let input = StatusInput::default();
        let cfg = Config::from_env();
        let ctx = ctx_with_today(&input, &cfg, Some(TodayRollup::default()));
        assert!(render(&ctx).is_none());
    }

    #[test]
    fn full_renders_money_and_tokens() {
        let input = StatusInput::default();
        let cfg = Config::from_env();
        let rollup = TodayRollup {
            day_anchor_ms: 0,
            cost_usd: 12.40,
            total_tokens: 1_200_000,
        };
        let ctx = ctx_with_today(&input, &cfg, Some(rollup));
        let seg = render(&ctx).unwrap();
        assert_eq!(strip(&seg.full), "today $12.4 1.2m");
        assert_eq!(strip(seg.compact.as_ref().unwrap()), "td $12 1.2m");
        assert_eq!(strip(seg.micro.as_ref().unwrap()), "td $12");
    }

    #[test]
    fn tokens_only_renders_without_dollar() {
        let input = StatusInput::default();
        let cfg = Config::from_env();
        let rollup = TodayRollup {
            day_anchor_ms: 0,
            cost_usd: 0.0,
            total_tokens: 500_000,
        };
        let ctx = ctx_with_today(&input, &cfg, Some(rollup));
        let seg = render(&ctx).unwrap();
        // Without $, full = "today · 500k". Compact stays consistent.
        let full = strip(&seg.full);
        assert!(full.starts_with("today"));
        assert!(full.contains("500k"));
        assert!(!full.contains('$'));
    }
}
