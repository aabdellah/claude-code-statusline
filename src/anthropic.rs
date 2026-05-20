//! Anthropic status page integration (status.claude.com).
//!
//! Cached 5 min in /tmp; refreshed by a detached background curl on miss/stale
//! so the statusline itself never blocks on network. Self-healing across renders.
//!
//! Race-free across concurrent CC sessions:
//!   1. Each session's curl writes to `/tmp/cc-anthropic-status.json.<pid>.tmp`
//!   2. On the next render, every session reconciles any tmp files by:
//!      - validating their JSON
//!      - atomically renaming valid ones onto the cache path
//!      - unlinking invalid ones
//!   No `sh -c` shell composition needed — `curl` is invoked directly with
//!   args, the rename is done by `fs::rename` (atomic POSIX rename).

use std::fs;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

const CACHE_PATH: &str = "/tmp/cc-anthropic-status.json";
const TMP_PREFIX: &str = "cc-anthropic-status.json.";
const TMP_SUFFIX: &str = ".tmp";
const TTL: Duration = Duration::from_secs(5 * 60);

/// Returns `None` when operational/unknown, or one of "minor" / "major" /
/// "critical" when degraded.
pub fn anthropic_status() -> Option<String> {
    reconcile_pending_fetches();
    let (cached, stale) = read_cache();
    if stale {
        spawn_background_fetch();
    }
    let cached = cached?;
    let indicator = cached.get("status")?.get("indicator")?.as_str()?;
    if indicator == "none" { None } else { Some(indicator.to_string()) }
}

fn read_cache() -> (Option<serde_json::Value>, bool) {
    let metadata = match fs::metadata(CACHE_PATH) {
        Ok(m) => m,
        Err(_) => return (None, true),
    };
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let age = SystemTime::now().duration_since(modified).unwrap_or(TTL);
    let stale = age >= TTL;

    let parsed = fs::read(CACHE_PATH)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());

    (parsed, stale)
}

/// Walk `/tmp` for pending tmp files dropped by previous-render background
/// curls. Validate each; promote valid ones to the cache via atomic rename;
/// unlink invalid/partial ones.
fn reconcile_pending_fetches() {
    let Ok(entries) = fs::read_dir("/tmp") else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if !name.starts_with(TMP_PREFIX) || !name.ends_with(TMP_SUFFIX) {
            continue;
        }
        // Reject symlinks immediately — defense against symlink TOCTOU where
        // an attacker in a world-writable /tmp swaps a tmp file for a symlink
        // pointing at a sensitive file we'd then promote into the cache via
        // atomic rename. symlink_metadata() does NOT follow symlinks.
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                let _ = fs::remove_file(&path);
                continue;
            }
            Ok(_) => {}
            Err(_) => continue,
        }
        // Only reconcile files older than 2s — anything fresher might still
        // be mid-write by an in-flight curl, and we'd rather wait for the
        // next render than promote a half-written file.
        if let Ok(meta) = path.metadata() {
            if let Ok(modified) = meta.modified() {
                if SystemTime::now().duration_since(modified)
                    .map(|d| d < Duration::from_secs(2))
                    .unwrap_or(false)
                {
                    continue;
                }
            }
        }
        match fs::read(&path) {
            Ok(bytes) if serde_json::from_slice::<serde_json::Value>(&bytes).is_ok() => {
                // Atomic POSIX rename — never produces a half-visible cache file.
                let _ = fs::rename(&path, CACHE_PATH);
            }
            _ => {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

/// Spawn a detached `curl` that writes the status JSON to a PID-suffixed tmp
/// file. Doesn't wait for the curl to finish — the next render's
/// `reconcile_pending_fetches()` will promote the file to the cache once
/// curl exits successfully.
fn spawn_background_fetch() {
    let tmp = format!("{}{}{}", CACHE_PATH, ".", std::process::id());
    let tmp_full = format!("{}{}", tmp, TMP_SUFFIX);

    // SAFETY: pre_exec runs between fork and exec. Calling setsid() there is
    // safe — it just detaches us from the parent process group so the child
    // outlives this render even when the controlling terminal is closed.
    let _ = unsafe {
        Command::new("curl")
            .args([
                "-sL",
                "-m", "5",
                "-o", &tmp_full,
                "https://status.claude.com/api/v2/status.json",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .pre_exec(|| {
                libc::setsid();
                Ok(())
            })
            .spawn()
    };
}
