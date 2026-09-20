//! Env-var configuration + per-render timing instrumentation.
//!
//! All env vars are read once at startup into a `Config` struct and passed
//! through the render pipeline by reference. No global state, no thread-local
//! magic — just a value that flows down.

use std::collections::HashSet;
use std::env;
use std::sync::Mutex;
use std::time::Instant;

/// Render mode.
/// - `Auto` (default): adaptive layout. The fitter starts from FULL variants
///   and downgrades lowest-priority segments to compact/micro/dropped until
///   the line fits the detected terminal width. See `layout.rs`.
/// - `Full`: force every segment to its FULL variant regardless of width.
/// - `Compact`: force every segment to its smallest available variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Full,
    Compact,
}

impl Mode {
    fn from_env() -> Self {
        match env::var("STATUSLINE_MODE").ok().as_deref() {
            Some("full") => Mode::Full,
            Some("compact") => Mode::Compact,
            _ => Mode::Auto,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub mode: Mode,
    pub hidden: HashSet<String>,
    pub width_override: Option<u16>,
    /// Total cells subtracted from detected terminal width before fitting.
    /// Claude Code's chat UI draws 2 cells of frame on EACH side of the
    /// pane (so 4 cells of invisible padding total). Default `4` accounts
    /// for this — tmux reports the full pane width, which is wider than
    /// CC's actual content area. Override via STATUSLINE_WIDTH_MARGIN.
    pub width_margin: u16,
    pub debug_timing: bool,
    pub debug_width: bool,
    pub show_plugins: bool,
    /// Disable boss-fight blink at >=90% context — for terminals where ANSI
    /// blink is jarring or unsupported.
    pub no_blink: bool,
    /// Claude Code's autocompact knobs, read from the env CC exports to the
    /// statusline process (its `settings.json` `env` block lands here).
    pub autocompact: AutoCompact,
}

/// The inputs Claude Code itself uses to decide when to auto-compact.
/// `context_window_size` on stdin is the MODEL window; compaction fires
/// far earlier, against `window - output_reserve - buffer`. See
/// `AutoCompact::trigger`.
#[derive(Debug, Clone, Default)]
pub struct AutoCompact {
    /// `DISABLE_AUTO_COMPACT` / `DISABLE_COMPACT` set: CC never compacts,
    /// so the model window is the only limit that matters.
    pub disabled: bool,
    /// `CLAUDE_CODE_AUTO_COMPACT_WINDOW` — user-chosen effective window,
    /// smaller than the model window. `None` = use the model window.
    pub window: Option<u64>,
    /// `CLAUDE_CODE_MAX_OUTPUT_TOKENS` — only matters when set BELOW the
    /// 20k reserve cap; every current model's default is above it.
    pub max_output_tokens: Option<u64>,
}

impl AutoCompact {
    /// CC reserves this many tokens for the model's reply (capped at the
    /// model's max output, which is always higher for current models).
    pub const OUTPUT_RESERVE: u64 = 20_000;
    /// Hardcoded compaction buffer in CC's threshold function.
    pub const BUFFER: u64 = 13_000;

    pub fn from_env() -> Self {
        let truthy = |k: &str| {
            matches!(
                env::var(k).ok().as_deref().map(|v| v.trim().to_ascii_lowercase()).as_deref(),
                Some("1") | Some("true") | Some("yes") | Some("on")
            )
        };
        let num = |k: &str| {
            env::var(k)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .filter(|&n| n > 0)
        };
        Self {
            disabled: truthy("DISABLE_AUTO_COMPACT") || truthy("DISABLE_COMPACT"),
            window: num("CLAUDE_CODE_AUTO_COMPACT_WINDOW"),
            max_output_tokens: num("CLAUDE_CODE_MAX_OUTPUT_TOKENS"),
        }
    }

    /// Token count at which CC auto-compacts, given the model window CC
    /// reported on stdin. `None` when autocompact is off or the numbers
    /// don't leave a positive threshold. Mirrors CC v2.1.278:
    /// `effective_window - min(max_output, 20k) - 13k`, where
    /// `effective_window` is `CLAUDE_CODE_AUTO_COMPACT_WINDOW` if set, else
    /// the model window.
    pub fn trigger(&self, model_window: u64) -> Option<u64> {
        if self.disabled || model_window == 0 {
            return None;
        }
        let effective = self.window.unwrap_or(model_window).min(model_window);
        let reserve = self
            .max_output_tokens
            .map_or(Self::OUTPUT_RESERVE, |m| m.min(Self::OUTPUT_RESERVE));
        let trigger = effective.checked_sub(reserve)?.checked_sub(Self::BUFFER)?;
        (trigger > 0).then_some(trigger)
    }
}

impl Config {
    pub fn from_env() -> Self {
        let hidden = env::var("STATUSLINE_HIDE")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let width_override = env::var("STATUSLINE_WIDTH")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n: &u16| n > 0);

        let width_margin = env::var("STATUSLINE_WIDTH_MARGIN")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4);

        Self {
            mode: Mode::from_env(),
            hidden,
            width_override,
            width_margin,
            debug_timing: env::var("STATUSLINE_DEBUG_TIMING").as_deref() == Ok("1"),
            debug_width: env::var("STATUSLINE_DEBUG_WIDTH").as_deref() == Ok("1"),
            show_plugins: env::var("STATUSLINE_SHOW_PLUGINS").as_deref() == Ok("1"),
            no_blink: env::var("STATUSLINE_NO_BLINK").as_deref() == Ok("1"),
            autocompact: AutoCompact::from_env(),
        }
    }

    pub fn is_hidden(&self, segment_id: &str) -> bool {
        self.hidden.contains(segment_id)
    }
}

// --- Per-render timing instrumentation ---------------------------------------

/// Records the elapsed time of `f()` under `name` when debug_timing is on.
/// When off, this collapses to a direct call with zero overhead.
pub fn timed<T, F: FnOnce() -> T>(name: &'static str, debug: bool, f: F) -> T {
    if !debug {
        return f();
    }
    let start = Instant::now();
    let r = f();
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    timings_lock().push((name, ms));
    r
}

// Process-wide timings buffer. A Mutex<Vec<_>> is fine here — statusline
// renders are single-threaded so contention is zero. The Mutex exists just
// to satisfy Rust's interior-mutability rules for the static.
pub(crate) static TIMINGS: Mutex<Vec<(&'static str, f64)>> = Mutex::new(Vec::new());

/// Lock TIMINGS while gracefully recovering from poisoning. In this binary
/// the Mutex is only contended by a single thread, so `unwrap()` would
/// never fire in practice — but `into_inner()` makes the recovery semantics
/// explicit, and avoids ever panicking on an instrumentation path.
fn timings_lock() -> std::sync::MutexGuard<'static, Vec<(&'static str, f64)>> {
    TIMINGS.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn reset_timings() {
    timings_lock().clear();
}

pub(crate) fn drain_timings() -> Vec<(&'static str, f64)> {
    std::mem::take(&mut *timings_lock())
}

#[cfg(test)]
mod autocompact_tests {
    use super::AutoCompact;

    fn ac(window: Option<u64>, max_out: Option<u64>) -> AutoCompact {
        AutoCompact { disabled: false, window, max_output_tokens: max_out }
    }

    #[test]
    fn trigger_is_window_minus_reserve_and_buffer() {
        assert_eq!(ac(None, None).trigger(200_000), Some(167_000));
        assert_eq!(ac(None, None).trigger(1_000_000), Some(967_000));
    }

    #[test]
    fn autocompact_window_env_overrides_model_window() {
        // The observed real-world case: 150k autocompact window, 200k model.
        assert_eq!(ac(Some(150_000), None).trigger(200_000), Some(117_000));
    }

    #[test]
    fn autocompact_window_never_exceeds_model_window() {
        assert_eq!(ac(Some(500_000), None).trigger(200_000), Some(167_000));
    }

    #[test]
    fn max_output_below_cap_shrinks_the_reserve() {
        assert_eq!(ac(None, Some(8_000)).trigger(200_000), Some(179_000));
        // Above the cap it's clamped to the 20k reserve.
        assert_eq!(ac(None, Some(64_000)).trigger(200_000), Some(167_000));
    }

    #[test]
    fn disabled_or_degenerate_yields_none() {
        let off = AutoCompact { disabled: true, ..Default::default() };
        assert_eq!(off.trigger(200_000), None);
        assert_eq!(ac(None, None).trigger(0), None);
        assert_eq!(ac(Some(30_000), None).trigger(200_000), None);
    }
}
