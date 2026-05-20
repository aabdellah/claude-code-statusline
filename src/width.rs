//! Terminal width detection.
//!
//! Layered fallback, cheapest reliable source first:
//!   1. STATUSLINE_WIDTH (env override, for testing) — handled by config.rs
//!   2. stdout/stderr TIOCGWINSZ (cheap; usually fails since CC pipes stdout)
//!   3. tmux display -p '#{pane_width}' (live, exact, no shell config)
//!   4. Parent process chain walk + open(/dev/<tty>) + TIOCGWINSZ
//!   5. /dev/tty open + TIOCGWINSZ
//!   6. stty size </dev/tty subprocess
//!   7. $COLUMNS env var
//!   8. Per-PTY cache file `/tmp/cc-term-width-<tty>`
//!   9. Shared cache file `/tmp/cc-term-width`
//!   → None (caller assumes wide, renders full)

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};

use crate::config::Config;

/// Trace map for debug-mode diagnosis. Each fallback writes its outcome here.
type Trace = HashMap<&'static str, String>;

pub fn detect_term_width(cfg: &Config) -> Option<u16> {
    let mut trace: Trace = HashMap::new();
    let mut result: Option<u16> = None;

    if let Some(override_val) = cfg.width_override {
        trace.insert("override", override_val.to_string());
        result = Some(override_val);
    } else {
        // 1. Standard streams (stdout/stderr) via TIOCGWINSZ on raw fds.
        let stdout_fd = std::io::stdout().as_raw_fd();
        let stderr_fd = std::io::stderr().as_raw_fd();
        if let Some(c) = ioctl_winsize_cols(stdout_fd) {
            trace.insert("stdout", c.to_string());
            result = Some(c);
        } else if let Some(c) = ioctl_winsize_cols(stderr_fd) {
            trace.insert("stderr", c.to_string());
            result = Some(c);
        } else {
            trace.insert("stdout", "no-tty".into());
            trace.insert("stderr", "no-tty".into());
        }

        // 2. tmux direct query — authoritative for tmux pane width.
        if result.is_none() && std::env::var_os("TMUX").is_some() {
            if let Some(c) = query_tmux() {
                trace.insert("tmux_query", c.to_string());
                result = Some(c);
            } else {
                trace.insert("tmux_query", "failed".into());
            }
        }

        // 3. Walk parent process chain → open TTY device → ioctl.
        if result.is_none() {
            if let Some(c) = ancestor_walk(&mut trace) {
                result = Some(c);
            }
        }

        // 4. /dev/tty — usually ENXIO for CC's subprocess, but defensive.
        if result.is_none() {
            if let Ok(file) = OpenOptions::new().read(true).open("/dev/tty") {
                let fd = file.as_raw_fd();
                if let Some(c) = ioctl_winsize_cols(fd) {
                    trace.insert("dev_tty", c.to_string());
                    result = Some(c);
                } else {
                    trace.insert("dev_tty", "no-cols".into());
                }
            } else {
                trace.insert("dev_tty", "open-failed".into());
            }
        }

        // 5. stty size </dev/tty subprocess.
        if result.is_none() {
            if let Some(c) = stty_size() {
                trace.insert("stty", c.to_string());
                result = Some(c);
            } else {
                trace.insert("stty", "failed".into());
            }
        }

        // 6. $COLUMNS env var (rare; shells don't export it by default).
        if result.is_none() {
            if let Ok(s) = std::env::var("COLUMNS") {
                trace.insert("env_COLUMNS", s.clone());
                if let Ok(n) = s.parse::<u16>() {
                    if n > 0 { result = Some(n); }
                }
            } else {
                trace.insert("env_COLUMNS", "unset".into());
            }
        }

        // 7. Per-PTY cache file (shell hook writes /tmp/cc-term-width-<tty>).
        if result.is_none() {
            if let Some(c) = read_per_pty_cache(&mut trace) {
                result = Some(c);
            }
        }

        // 8. Single shared cache (legacy fallback).
        if result.is_none() {
            if let Ok(s) = fs::read_to_string("/tmp/cc-term-width") {
                if let Ok(n) = s.trim().parse::<u16>() {
                    if n > 0 {
                        trace.insert("shared_cache", n.to_string());
                        result = Some(n);
                    }
                }
            } else {
                trace.insert("shared_cache", "missing".into());
            }
        }
    }

    if cfg.debug_width {
        let payload = serde_json::json!({
            "detected": result,
            "trace": trace,
            "pid": std::process::id(),
            "ts": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        });
        let _ = fs::write("/tmp/cc-statusline-width.json",
                          serde_json::to_string_pretty(&payload).unwrap());
    }

    result
}

/// SAFETY: ioctl(TIOCGWINSZ, &winsize) is a read-only operation that the
/// kernel fills in. Passing a zero-initialized winsize is correct — ioctl
/// will overwrite all fields on success. Returns None if the fd has no
/// associated TTY (errno ENOTTY) or ioctl errors otherwise.
fn ioctl_winsize_cols(fd: i32) -> Option<u16> {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        let r = libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws);
        if r == 0 && ws.ws_col > 0 { Some(ws.ws_col) } else { None }
    }
}

/// Ask tmux for THIS pane's width — the one CC is running in, not the
/// pane that happens to be focused right now.
///
/// Without `-t <target>`, `tmux display -p` operates on the focused pane,
/// which is rarely what you want from a subprocess. Tmux sets `TMUX_PANE`
/// in the subprocess env to identify the pane that spawned us; we target
/// that explicitly. Fall back to the focused pane only if `TMUX_PANE` is
/// somehow unset (shouldn't happen inside a real tmux session).
fn query_tmux() -> Option<u16> {
    let mut cmd = Command::new("tmux");
    cmd.arg("display").arg("-p");
    if let Ok(pane) = std::env::var("TMUX_PANE") {
        cmd.arg("-t").arg(&pane);
    }
    cmd.arg("#{pane_width}")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let out = cmd.output().ok()?;
    if !out.status.success() { return None; }
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

fn stty_size() -> Option<u16> {
    // stty needs a TTY on its stdin. Open /dev/tty and pipe it in.
    let tty = OpenOptions::new().read(true).open("/dev/tty").ok()?;
    let out = Command::new("stty")
        .arg("size")
        .stdin(Stdio::from(tty))
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() { return None; }
    let s = String::from_utf8(out.stdout).ok()?;
    // Output format: "<rows> <cols>"
    let parts: Vec<&str> = s.split_whitespace().collect();
    parts.get(1)?.parse().ok()
}

/// Walk the parent process chain looking for an ancestor with a controlling
/// TTY, then open that PTY device and ioctl(TIOCGWINSZ). Works without any
/// shell config because we go around the controlling-terminal restriction by
/// opening the device file directly. Covers local terminals, tmux (each pane
/// has its own PTY), and SSH (remote PTY is the same to the kernel).
fn ancestor_walk(trace: &mut Trace) -> Option<u16> {
    let mut walk_pid = unsafe { libc::getppid() };
    let mut walk_log: Vec<String> = Vec::new();

    for _ in 0..8 {
        if walk_pid <= 1 { break; }
        let out = Command::new("ps")
            .args(["-o", "tty=,ppid=", "-p", &walk_pid.to_string()])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let s = String::from_utf8(out.stdout).ok()?;
        let trimmed = s.trim();
        if trimmed.is_empty() { break; }
        let mut parts = trimmed.split_whitespace();
        let tty_name = parts.next().unwrap_or("");
        let next_pid: i32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);

        if tty_name != "?" && tty_name != "??" && tty_name != "-" && !tty_name.is_empty() {
            let tty_path = if tty_name.starts_with('/') {
                tty_name.to_string()
            } else {
                format!("/dev/{}", tty_name)
            };
            match OpenOptions::new().read(true).write(true).open(&tty_path) {
                Ok(file) => {
                    let fd = file.as_raw_fd();
                    if let Some(cols) = ioctl_winsize_cols(fd) {
                        walk_log.push(format!("pid={} tty={} cols={}", walk_pid, tty_name, cols));
                        trace.insert("ancestor_walk", walk_log.join("; "));
                        return Some(cols);
                    } else {
                        walk_log.push(format!("pid={} tty={} cols=?", walk_pid, tty_name));
                    }
                }
                Err(e) => {
                    walk_log.push(format!("pid={} tty={} open-error={}", walk_pid, tty_name, e.kind() as i32));
                }
            }
        } else {
            walk_log.push(format!("pid={} no-tty", walk_pid));
        }

        if next_pid <= 1 { break; }
        walk_pid = next_pid;
    }
    if !walk_log.is_empty() {
        trace.insert("ancestor_walk", walk_log.join("; "));
    } else {
        trace.insert("ancestor_walk", "no-ancestors".into());
    }
    None
}

fn read_per_pty_cache(trace: &mut Trace) -> Option<u16> {
    let out = Command::new("ps")
        .args(["-o", "tty=", "-p", &unsafe { libc::getppid() }.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let tty_raw = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if tty_raw.is_empty() || tty_raw == "?" || tty_raw == "??" || tty_raw == "-" {
        trace.insert("caller_tty", format!("none (ps='{}')", tty_raw));
        return None;
    }
    let tty_key = tty_raw.replace('/', "-");
    trace.insert("caller_tty", tty_key.clone());
    let cache_path = format!("/tmp/cc-term-width-{}", tty_key);
    let content = fs::read_to_string(&cache_path).ok()?;
    let n: u16 = content.trim().parse().ok()?;
    if n > 0 {
        trace.insert("tty_cache", n.to_string());
        Some(n)
    } else {
        None
    }
}

// Re-exported so the timing flush can call it without re-importing Write trait
#[allow(dead_code)]
fn _import_anchor(w: &mut dyn Write) -> std::io::Result<()> {
    write!(w, "")
}
