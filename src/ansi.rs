//! ANSI styling primitives + gradient bar.
//!
//! 24-bit truecolor is used for the context gradient; terminals that don't
//! support it simply ignore the escape sequences — the text remains legible.

use std::fmt::Write;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

// SGR codes. `&'static str` so we can concatenate them with format!/write!
// without allocating per-call.
pub const RESET: &str = "\x1b[0m";
pub const DIM: &str = "\x1b[2m";
pub const BOLD: &str = "\x1b[1m";
pub const ITALIC: &str = "\x1b[3m";

pub const RED: &str = "\x1b[31m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const BLUE: &str = "\x1b[34m";
pub const MAGENTA: &str = "\x1b[35m";
pub const CYAN: &str = "\x1b[36m";
pub const GRAY: &str = "\x1b[90m";

pub const BRIGHT_MAGENTA: &str = "\x1b[95m";

// 256-color variants for the mode words.
pub const VIOLET: &str = "\x1b[38;5;141m"; // soft purple — "thinking" mode
pub const AMBER: &str = "\x1b[38;5;215m";  // distinct from yellow — output-style word

pub const BLINK: &str = "\x1b[5m";

/// CSI-sequence pattern for stripping ANSI when measuring visible length.
/// We don't emit OSC links yet, so handling CSI alone is sufficient.
fn visible_length_internal(s: &str) -> usize {
    // Manual single-pass scanner — faster than spawning a regex compile for
    // something this simple. State: 0 = normal, 1 = saw ESC, 2 = inside CSI.
    let mut state = 0u8;
    let mut count = 0usize;
    for b in s.bytes() {
        match state {
            0 => {
                if b == 0x1b { state = 1; }
                else if b < 0x80 { count += 1; }
                else if b & 0xC0 != 0x80 { count += 1; } // leading byte of UTF-8 char
                // continuation bytes (10xxxxxx) don't count — they're part of
                // the previous codepoint
            }
            1 => state = if b == b'[' { 2 } else { 0 }, // ESC [
            2 => {
                // CSI terminates on a byte in 0x40..=0x7E (final byte).
                if (0x40..=0x7E).contains(&b) { state = 0; }
            }
            _ => unreachable!(),
        }
    }
    count
}

pub fn visible_length(s: &str) -> usize {
    visible_length_internal(s)
}

/// 24-bit truecolor RGB foreground sequence builder.
/// Returns a heap String — escape sequences are tiny so allocation is cheap.
pub fn rgb(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[38;2;{};{};{}m", r, g, b)
}

/// Linear gradient green(0,255,0) → yellow(255,255,0) → red(255,0,0) for t in [0,1].
/// Returns (r,g,b) as u8 triplet, gamma-correct enough for terminal display.
pub fn gradient(t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    if t <= 0.5 {
        ((510.0 * t).round() as u8, 255, 0)
    } else {
        (255, (255.0 * (1.0 - (t - 0.5) * 2.0)).round() as u8, 0)
    }
}

pub fn grad_color(t: f32) -> String {
    let (r, g, b) = gradient(t);
    rgb(r, g, b)
}

/// Wrap `s` in the gradient color at position `t`, then RESET.
pub fn grad_text(s: &str, t: f32) -> String {
    format!("{}{}{}", grad_color(t), s, RESET)
}

/// Standard threshold color (gradient would be overkill for rate-limit values).
/// Returns a `&'static str` since these are all const SGR codes.
pub fn pct_color(pct: f64, warn_at: f64, crit_at: f64) -> &'static str {
    if pct >= crit_at { RED }
    else if pct >= warn_at { YELLOW }
    else if pct >= 40.0 { GREEN }
    else { GRAY }
}

// Tiny deterministic-enough RNG seeded from process start time. Used only for
// the boss-fight bar flicker — no need for crypto strength. SplitMix64 because
// it's 1 multiply + 1 xorshift, near-free.
fn rand_bool_35pct() -> bool {
    static STATE: OnceLock<std::sync::Mutex<u64>> = OnceLock::new();
    let mutex = STATE.get_or_init(|| {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15)
            ^ std::process::id() as u64;
        std::sync::Mutex::new(seed)
    });
    let mut s = mutex.lock().unwrap();
    *s = s.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^= z >> 31;
    // 35% threshold on the top byte
    (z & 0xFF) < 90
}

/// 10-cell bar with per-cell gradient coloring on filled cells.
/// Empty cells are explicitly reset-then-dim so they don't inherit the last
/// filled cell's RGB. Above 85% the bar enters "boss-fight" mode — filled
/// cells become solid crit-red, empty cells switch to a damaged-bar character
/// (`▒`). At ≥90%, an ANSI blink + per-render █/▓ flicker is added.
///
/// `no_blink` disables the ANSI blink (e.g. for terminals where it's jarring
/// or unsupported). Propagated from `Config` so the env-var read happens
/// once at startup, not on every render.
pub fn gradient_bar(pct: f64, width: usize, no_blink: bool) -> String {
    let p = pct.clamp(0.0, 100.0);
    let filled = ((p / 100.0) * width as f64).round() as usize;
    let critical = p >= 85.0;
    let extreme = p >= 90.0;

    let mut out = String::with_capacity(width * 16);

    if critical {
        // ≥90% adds two layered effects:
        //   (a) ANSI blink `\x1b[5m` — terminal-driven, ~2 Hz, independent of CC renders.
        //   (b) Render-driven flicker: filled cells randomly mix █/▓ each render.
        if extreme && !no_blink {
            out.push_str(BLINK);
        }
        out.push_str(BOLD);
        out.push_str(RED);
        for _ in 0..filled {
            if extreme && rand_bool_35pct() {
                out.push('▓');
            } else {
                out.push('█');
            }
        }
        if filled < width {
            out.push_str(DIM);
            for _ in filled..width {
                out.push('▒');
            }
        }
        out.push_str(RESET);
    } else {
        for i in 0..filled {
            let t = if width == 1 { 1.0 } else { i as f32 / (width as f32 - 1.0) };
            write!(out, "{}", grad_color(t)).unwrap();
            out.push('█');
        }
        if filled < width {
            out.push_str(RESET);
            out.push_str(DIM);
            for _ in filled..width {
                out.push('░');
            }
        }
        out.push_str(RESET);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_length_strips_csi() {
        assert_eq!(visible_length("hello"), 5);
        assert_eq!(visible_length("\x1b[36mhello\x1b[0m"), 5);
        assert_eq!(visible_length("\x1b[1;31mfoo\x1b[0m bar"), 7);
        // Multi-byte UTF-8 should count as one char each
        assert_eq!(visible_length("█·█"), 3);
    }

    #[test]
    fn gradient_endpoints() {
        // t=0 → pure green; t=1 → pure red; t=0.5 → pure yellow
        assert_eq!(gradient(0.0), (0, 255, 0));
        assert_eq!(gradient(1.0), (255, 0, 0));
        assert_eq!(gradient(0.5), (255, 255, 0));
    }

    #[test]
    fn pct_color_thresholds() {
        assert_eq!(pct_color(95.0, 65.0, 85.0), RED);
        assert_eq!(pct_color(70.0, 65.0, 85.0), YELLOW);
        assert_eq!(pct_color(50.0, 65.0, 85.0), GREEN);
        assert_eq!(pct_color(10.0, 65.0, 85.0), GRAY);
    }

    #[test]
    fn gradient_bar_renders_filled_proportional() {
        let bar = gradient_bar(50.0, 10, false);
        // Bar contains '█' for filled, '░' for empty — and ANSI escapes around them.
        let visible = bar.chars().filter(|c| *c == '█' || *c == '░').count();
        assert_eq!(visible, 10);
    }
}
