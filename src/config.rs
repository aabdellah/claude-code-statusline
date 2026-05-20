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
    TIMINGS.lock().unwrap().push((name, ms));
    r
}

// Process-wide timings buffer. A Mutex<Vec<_>> is fine here — statusline
// renders are single-threaded so contention is zero. The Mutex exists just
// to satisfy Rust's interior-mutability rules for the static.
pub static TIMINGS: Mutex<Vec<(&'static str, f64)>> = Mutex::new(Vec::new());

pub fn reset_timings() {
    TIMINGS.lock().unwrap().clear();
}

pub fn drain_timings() -> Vec<(&'static str, f64)> {
    std::mem::take(&mut *TIMINGS.lock().unwrap())
}
