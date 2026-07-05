//! Rate limits — 5h + weekly windows with usage % and projection.
//!   full     "5h 64%→2h15m 7d 71% →98%"            mid-week
//!   full     "5h 64%→1h2m 7d 90% →22h →100%"       last 24h of the 7d window
//!   full     "5h 64% 7d 71% →98% fable 42% →88%"   Fable 5 dedicated weekly
//!   compact  "5h:64 7d:71/98"                      mid-week
//!   compact  "5h:64 7d:90/22h/100 f5:42/88"        last 24h + Fable
//!
//! Weekly windows beyond the account-wide `7d`: `seven_day_overage_included`
//! (Fable 5's dedicated limit), `seven_day_opus`, `seven_day_sonnet` — all
//! rendered with the same pace projection. Sourced from stdin when CC sends
//! them; until then the oauth usage cache (usage.rs) fills them in.
//!
//! Contributes red signals: 5h ≥90%, any weekly ≥90%, projection over 115%.
//! The underpace red (projected <80% late in the week) fires ONLY for the
//! account-wide 7d window: "wasted headroom" is a goal for the weekly
//! budget, not for per-model limits the user may never intend to consume —
//! an idle scoped window projects to 0% and would false-red every late-week
//! render. Total reds cap at 3 (layout.rs's per-segment invariant) now that
//! up to five windows can render.

use crate::ansi::{self, DIM, GREEN, RESET};
use crate::context::RenderContext;
use crate::format::fmt_reset_time;
use crate::input::RateLimitWindow;
use crate::layout::{Priority, Seg};
use crate::pace;
use crate::repr;

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    // stdin is the primary source; the oauth usage cache fills in the
    // model-scoped weeklies CC doesn't forward yet (see input.rs). No
    // early-return on a missing stdin `rate_limits` — the oauth windows
    // can carry the segment alone.
    let rl = ctx.input.rate_limits.as_ref();
    let oauth = &ctx.oauth_scoped;

    let mut full_bits: Vec<String> = Vec::new();
    let mut compact_bits: Vec<String> = Vec::new();
    let mut red_count = 0u32;

    // 5h window
    if let Some(fh) = rl.and_then(|r| r.five_hour.as_ref()) {
        if let Some(p) = fh.used_percentage {
            let col = ansi::pct_color(p, 70.0, 90.0);
            let (mut full, compact) = repr::percent("5h", "5h", p, col);
            let reset_str = fh.resets_at.as_ref().map(fmt_reset_time).unwrap_or_default();
            if !reset_str.is_empty() {
                full.push_str(&format!("{}→{}{}", DIM, reset_str, RESET));
            }
            full_bits.push(full);
            compact_bits.push(compact);
            if p >= 90.0 {
                red_count += 1;
            }
        }
    }

    // Weekly windows: account-wide 7d first, then the model-scoped weeklies.
    // `seven_day_overage_included` is Fable 5's dedicated limit (see input.rs).
    // Scoped windows fall back to the oauth usage cache when stdin lacks a
    // USABLE window (`prefer` skips a present-but-drifted stdin window with
    // no used_percentage, rather than letting it block the fallback). The
    // bool marks the account-wide window — the only one where underpace is a
    // red-worthy signal.
    let weeklies = [
        ("7d", "7d", rl.and_then(|r| r.seven_day.as_ref()), true),
        (
            "fable",
            "f5",
            prefer(
                rl.and_then(|r| r.seven_day_overage_included.as_ref()),
                oauth.fable.as_ref(),
            ),
            false,
        ),
        (
            "opus",
            "op",
            prefer(rl.and_then(|r| r.seven_day_opus.as_ref()), oauth.opus.as_ref()),
            false,
        ),
        (
            "sonnet",
            "so",
            prefer(rl.and_then(|r| r.seven_day_sonnet.as_ref()), oauth.sonnet.as_ref()),
            false,
        ),
    ];
    for (label_full, label_compact, win, account_wide) in weeklies {
        let Some(win) = win else { continue };
        let Some(pace_obj) = pace::seven_day_pace(win) else { continue };
        let (full, compact, reds) = weekly_window(label_full, label_compact, win, &pace_obj, account_wide);
        full_bits.push(full);
        compact_bits.push(compact);
        red_count += reds;
    }

    if full_bits.is_empty() {
        return None;
    }

    // Critical: rate limits are recovery-relevant — losing them on narrow
    // terminals defeats the whole point of glanceable quota awareness.
    // Same tier as model + context; the fitter will downgrade siblings
    // (cost/cache/etc) before touching this segment's variants. No `micro`
    // variant: STATUSLINE_MODE=compact prefers micro over compact, so a
    // micro here would hide the 7d window for compact-mode users even on
    // wide terminals. The compact variant is the narrow floor.
    // Reds cap at 3: the weekly windows are correlated (account-wide 7d
    // includes model-scoped usage), and layout.rs documents ≤3 per segment.
    Some(
        Seg::new("rate-limits", Priority::Critical, full_bits.join(" "))
            .with_compact(compact_bits.join(" "))
            .red_n(red_count.min(3)),
    )
}

/// Prefer the stdin window over the oauth fallback, but only when stdin's
/// window actually carries a usage %. A present-but-drifted stdin window
/// (e.g. CC sends `{"resets_at": ...}` with no percentage) would otherwise
/// shadow a perfectly good oauth value and blank the window.
fn prefer<'a>(
    stdin: Option<&'a RateLimitWindow>,
    oauth: Option<&'a RateLimitWindow>,
) -> Option<&'a RateLimitWindow> {
    match stdin {
        Some(w) if w.used_percentage.is_some() => Some(w),
        _ => oauth,
    }
}

/// Render one weekly (7-day) window: usage %, reset countdown in the last
/// 24h, and pace projection. Shared by the account-wide `7d` window and the
/// model-scoped weeklies (Fable/Opus/Sonnet) — they're all rolling 7-day
/// windows, so the same pace math applies. `account_wide` gates the
/// underpace red signal (see module doc). Returns (full, compact, reds).
fn weekly_window(
    label_full: &str,
    label_compact: &str,
    win: &RateLimitWindow,
    pace_obj: &pace::Pace,
    account_wide: bool,
) -> (String, String, u32) {
    let mut reds = 0u32;
    let used_col = ansi::pct_color(pace_obj.used_pct, 70.0, 90.0);
    let (mut full, mut compact) = repr::percent(label_full, label_compact, pace_obj.used_pct, used_col);

    // In the last 24h of the window, render the reset countdown BEFORE the
    // projection — recovery is the actionable signal at this point. GREEN to
    // communicate "almost there, hold on".
    if pace_obj.in_last_24h() {
        if let Some(reset_str) = win
            .resets_at
            .as_ref()
            .map(fmt_reset_time)
            .filter(|s| !s.is_empty())
        {
            full.push_str(&format!(" {}→{}{}", GREEN, reset_str, RESET));
            compact.push_str(&format!("{}/{}{}", GREEN, reset_str, RESET));
        }
    }

    if let (Some(projected), Some(frac)) = (pace_obj.projected, pace_obj.frac_elapsed) {
        let pcol = pace::pace_color(projected, frac);
        // `~` prefix during the volatile early-window period (frac < 0.10):
        // the projection is real and worth showing (day-1 overspend triggers
        // red), but the exact number swings hour-to-hour until ~17h into the
        // window.
        let marker = if pace_obj.is_volatile() { "~" } else { "" };
        full.push_str(&format!(
            " {}→{}{}%{}",
            pcol,
            marker,
            projected.round() as i64,
            RESET
        ));
        compact.push_str(&format!(
            "{}/{}{}{}",
            pcol,
            marker,
            projected.round() as i64,
            RESET
        ));
        if projected > 115.0 || (account_wide && projected < 80.0 && frac >= 0.70) {
            reds += 1;
        }
    }
    if pace_obj.used_pct >= 90.0 {
        reds += 1;
    }
    (full, compact, reds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::strip_ansi;
    use crate::config::Config;
    use crate::context::RenderContext;
    use crate::input::{RateLimitWindow, RateLimits, StatusInput};

    fn input_with_rate_limits(five_pct: f64, seven_pct: f64) -> StatusInput {
        let mut input = StatusInput::default();
        input.rate_limits = Some(RateLimits {
            five_hour: Some(RateLimitWindow {
                used_percentage: Some(five_pct),
                resets_at: None,
            }),
            seven_day: Some(RateLimitWindow {
                used_percentage: Some(seven_pct),
                resets_at: None,
            }),
            ..Default::default()
        });
        input
    }

    fn with_fable(mut input: StatusInput, fable_pct: f64) -> StatusInput {
        input
            .rate_limits
            .as_mut()
            .expect("rate_limits set")
            .seven_day_overage_included = Some(RateLimitWindow {
            used_percentage: Some(fable_pct),
            resets_at: None,
        });
        input
    }

    #[test]
    fn five_hour_at_90pct_red_signal() {
        let input = input_with_rate_limits(90.0, 30.0);
        let cfg = Config::from_env();
        let ctx = RenderContext::test_default(&input, &cfg);
        let seg = render(&ctx).expect("renders");
        assert_eq!(seg.red_count, 1, "5h ≥90% contributes 1 red signal");
    }

    #[test]
    fn seven_day_at_90pct_red_signal() {
        let input = input_with_rate_limits(50.0, 90.0);
        let cfg = Config::from_env();
        let ctx = RenderContext::test_default(&input, &cfg);
        let seg = render(&ctx).expect("renders");
        // 7d ≥90% AND no projection (no resets_at) → 1 red signal.
        assert_eq!(seg.red_count, 1);
    }

    #[test]
    fn both_5h_and_7d_red_stack() {
        let input = input_with_rate_limits(95.0, 95.0);
        let cfg = Config::from_env();
        let ctx = RenderContext::test_default(&input, &cfg);
        let seg = render(&ctx).expect("renders");
        assert_eq!(seg.red_count, 2);
    }

    #[test]
    fn no_red_when_well_under_90pct() {
        let input = input_with_rate_limits(40.0, 50.0);
        let cfg = Config::from_env();
        let ctx = RenderContext::test_default(&input, &cfg);
        let seg = render(&ctx).expect("renders");
        assert_eq!(seg.red_count, 0);
    }

    #[test]
    fn hidden_when_rate_limits_missing() {
        let input = StatusInput::default();
        let cfg = Config::from_env();
        let ctx = RenderContext::test_default(&input, &cfg);
        assert!(render(&ctx).is_none());
    }

    #[test]
    fn full_variant_starts_with_5h_then_7d() {
        let input = input_with_rate_limits(20.0, 30.0);
        let cfg = Config::from_env();
        let ctx = RenderContext::test_default(&input, &cfg);
        let seg = render(&ctx).expect("renders");
        let stripped = strip_ansi(&seg.full);
        let idx_5h = stripped.find("5h ").expect("has 5h marker");
        let idx_7d = stripped.find("7d ").expect("has 7d marker");
        assert!(idx_5h < idx_7d, "5h appears before 7d in full variant");
    }

    #[test]
    fn fable_window_renders_after_7d_with_own_labels() {
        let input = with_fable(input_with_rate_limits(20.0, 30.0), 42.0);
        let cfg = Config::from_env();
        let ctx = RenderContext::test_default(&input, &cfg);
        let seg = render(&ctx).expect("renders");
        let full = strip_ansi(&seg.full);
        let idx_7d = full.find("7d ").expect("has 7d marker");
        let idx_fable = full.find("fable 42%").expect("has fable window");
        assert!(idx_7d < idx_fable, "7d appears before fable in full variant");
        let compact = strip_ansi(seg.compact.as_deref().expect("has compact variant"));
        assert!(compact.contains("f5:42"), "compact uses f5 label: {compact}");
    }

    /// Window with a resets_at N seconds from now (as unix-seconds number).
    fn win_resetting_in(pct: f64, secs_from_now: i64) -> RateLimitWindow {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        RateLimitWindow {
            used_percentage: Some(pct),
            resets_at: Some(serde_json::Value::Number(serde_json::Number::from(
                now + secs_from_now,
            ))),
        }
    }

    #[test]
    fn idle_scoped_windows_late_in_week_do_not_red() {
        // Idle model-scoped windows (0% used) late in the week project to 0%
        // — the account-wide underpace rule must NOT apply to them, or three
        // unused scoped windows alone would trip the CRIT banner on a
        // perfectly healthy session.
        let day_and_a_half = 36 * 3600; // frac ≈ 0.786, past the 0.70 gate
        let mut input = StatusInput::default();
        input.rate_limits = Some(RateLimits {
            five_hour: Some(RateLimitWindow { used_percentage: Some(20.0), resets_at: None }),
            // 70% used at frac .786 → projected ~89%, inside the safe band.
            seven_day: Some(win_resetting_in(70.0, day_and_a_half)),
            seven_day_overage_included: Some(win_resetting_in(0.0, day_and_a_half)),
            seven_day_opus: Some(win_resetting_in(0.0, day_and_a_half)),
            seven_day_sonnet: Some(win_resetting_in(0.0, day_and_a_half)),
        });
        let cfg = Config::from_env();
        let ctx = RenderContext::test_default(&input, &cfg);
        let seg = render(&ctx).expect("renders");
        assert_eq!(seg.red_count, 0, "idle scoped windows are not red-worthy");
    }

    #[test]
    fn account_wide_underpace_red_still_fires() {
        // The underpace red stays meaningful for the account-wide 7d window:
        // 40% used at frac ≈ 0.786 → projected ~51% < 80% late in the week.
        let mut input = StatusInput::default();
        input.rate_limits = Some(RateLimits {
            seven_day: Some(win_resetting_in(40.0, 36 * 3600)),
            ..Default::default()
        });
        let cfg = Config::from_env();
        let ctx = RenderContext::test_default(&input, &cfg);
        let seg = render(&ctx).expect("renders");
        assert_eq!(seg.red_count, 1, "7d underpace late in week is red");
    }

    #[test]
    fn red_count_caps_at_three_across_all_windows() {
        // Everything on fire: 5h ≥90 (1) + four weeklies each ≥90 AND
        // overpaced (2 each) = 9 raw reds. The weekly windows are correlated
        // (account-wide 7d contains the scoped usage), so the segment caps
        // at layout.rs's documented per-segment maximum of 3.
        let mid_window = 3 * 24 * 3600 + 12 * 3600; // frac = 0.5 → projected 190%
        let mut input = StatusInput::default();
        input.rate_limits = Some(RateLimits {
            five_hour: Some(RateLimitWindow { used_percentage: Some(95.0), resets_at: None }),
            seven_day: Some(win_resetting_in(95.0, mid_window)),
            seven_day_overage_included: Some(win_resetting_in(95.0, mid_window)),
            seven_day_opus: Some(win_resetting_in(95.0, mid_window)),
            seven_day_sonnet: Some(win_resetting_in(95.0, mid_window)),
        });
        let cfg = Config::from_env();
        let ctx = RenderContext::test_default(&input, &cfg);
        let seg = render(&ctx).expect("renders");
        assert_eq!(seg.red_count, 3, "reds cap at 3 per segment");
    }

    #[test]
    fn oauth_fable_fills_in_when_stdin_lacks_it() {
        // CC v2.1.201 stdin has no fable window — the oauth usage cache
        // supplies it and the segment renders it like a stdin window.
        let input = input_with_rate_limits(20.0, 30.0);
        let cfg = Config::from_env();
        let mut ctx = RenderContext::test_default(&input, &cfg);
        ctx.oauth_scoped.fable = Some(RateLimitWindow {
            used_percentage: Some(42.0),
            resets_at: None,
        });
        let seg = render(&ctx).expect("renders");
        let full = strip_ansi(&seg.full);
        assert!(full.contains("fable 42%"), "oauth fable renders: {full}");
    }

    #[test]
    fn stdin_fable_wins_over_oauth() {
        // stdin is realtime at the turn boundary; the oauth cache can be up
        // to MAX_RENDER_AGE stale. When both carry the window, stdin wins.
        let input = with_fable(input_with_rate_limits(20.0, 30.0), 42.0);
        let cfg = Config::from_env();
        let mut ctx = RenderContext::test_default(&input, &cfg);
        ctx.oauth_scoped.fable = Some(RateLimitWindow {
            used_percentage: Some(99.0),
            resets_at: None,
        });
        let seg = render(&ctx).expect("renders");
        let full = strip_ansi(&seg.full);
        assert!(full.contains("fable 42%"), "stdin value wins: {full}");
        assert!(!full.contains("fable 99%"), "oauth value ignored: {full}");
    }

    #[test]
    fn oauth_fable_renders_even_without_stdin_rate_limits() {
        // Fresh sessions send near-empty JSON — the oauth windows can carry
        // the segment alone.
        let input = StatusInput::default();
        let cfg = Config::from_env();
        let mut ctx = RenderContext::test_default(&input, &cfg);
        ctx.oauth_scoped.fable = Some(RateLimitWindow {
            used_percentage: Some(42.0),
            resets_at: None,
        });
        let seg = render(&ctx).expect("renders");
        assert!(strip_ansi(&seg.full).contains("fable 42%"));
    }

    #[test]
    fn no_micro_variant_so_compact_mode_keeps_all_windows() {
        // STATUSLINE_MODE=compact prefers micro over compact; a micro here
        // would hide the 7d window for compact-mode users. Guard against a
        // micro variant creeping back.
        let input = with_fable(input_with_rate_limits(20.0, 30.0), 42.0);
        let cfg = Config::from_env();
        let ctx = RenderContext::test_default(&input, &cfg);
        let seg = render(&ctx).expect("renders");
        assert!(seg.micro.is_none(), "rate-limits must not define a micro variant");
    }

    #[test]
    fn drifted_stdin_window_falls_back_to_oauth() {
        // A present-but-unusable stdin window (resets_at only, no
        // used_percentage) must NOT shadow a good oauth value.
        let mut input = input_with_rate_limits(20.0, 30.0);
        input.rate_limits.as_mut().unwrap().seven_day_overage_included =
            Some(RateLimitWindow { used_percentage: None, resets_at: None });
        let cfg = Config::from_env();
        let mut ctx = RenderContext::test_default(&input, &cfg);
        ctx.oauth_scoped.fable =
            Some(RateLimitWindow { used_percentage: Some(55.0), resets_at: None });
        let seg = render(&ctx).expect("renders");
        assert!(strip_ansi(&seg.full).contains("fable 55%"), "oauth fills the gap");
    }

    #[test]
    fn fable_window_at_90pct_adds_red_signal() {
        let input = with_fable(input_with_rate_limits(20.0, 30.0), 90.0);
        let cfg = Config::from_env();
        let ctx = RenderContext::test_default(&input, &cfg);
        let seg = render(&ctx).expect("renders");
        assert_eq!(seg.red_count, 1, "fable ≥90% contributes 1 red signal");
    }

    #[test]
    fn fable_absent_output_unchanged() {
        // CC v2.1.201 doesn't send the scoped weeklies — the segment must
        // render exactly as before when only five_hour/seven_day are present.
        let input = input_with_rate_limits(20.0, 30.0);
        let cfg = Config::from_env();
        let ctx = RenderContext::test_default(&input, &cfg);
        let seg = render(&ctx).expect("renders");
        let full = strip_ansi(&seg.full);
        assert!(!full.contains("fable"), "no fable text when window absent");
        assert!(!full.contains("opus"), "no opus text when window absent");
        assert!(!full.contains("sonnet"), "no sonnet text when window absent");
    }
}
