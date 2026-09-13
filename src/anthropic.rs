//! Anthropic status page integration (status.claude.com).
//!
//! We read the per-component list (`components.json`) and look ONLY at the
//! "Claude Code" component. The page-wide `status.json` indicator turns
//! "minor"/"major" for ANY product on the page (claude.ai, Cowork, Console,
//! Government…), which made the segment nag about outages that could not
//! affect a CC session.
//!
//! Cached 5 min in the shared scratch dir (`/tmp` on Unix, `%TEMP%` on
//! Windows); refreshed by a detached background curl on miss/stale so the
//! statusline itself never blocks on network. Self-healing across renders.
//!
//! Race-free across concurrent CC sessions:
//!   1. Each session's curl writes to `<tmp>/cc-anthropic-components.json.<pid>.tmp`
//!   2. On the next render, every session reconciles any tmp files by:
//!      - validating their JSON
//!      - atomically renaming valid ones onto the cache path
//!      - unlinking invalid ones
//!
//! No `sh -c` shell composition needed — `curl` is invoked directly with
//! args, the rename is done by `fs::rename` (atomic POSIX rename).

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use crate::platform;

const CACHE_NAME: &str = "cc-anthropic-components.json";
const TMP_PREFIX: &str = "cc-anthropic-components.json.";
/// Statuspage component name we care about. Matched case-insensitively on
/// the trimmed name so cosmetic renames ("Claude Code (CLI)") keep matching
/// via prefix.
const COMPONENT_NAME: &str = "claude code";
const TMP_SUFFIX: &str = ".tmp";
const TTL: Duration = Duration::from_secs(5 * 60);

fn cache_path() -> PathBuf {
    platform::shared_tmp_dir().join(CACHE_NAME)
}

/// Returns `None` when the Claude Code component is operational / unknown,
/// or its Statuspage status string when degraded: one of
/// "degraded_performance" / "partial_outage" / "major_outage" /
/// "under_maintenance".
pub fn anthropic_status() -> Option<String> {
    reconcile_pending_fetches();
    let (cached, stale) = read_cache();
    if stale {
        spawn_background_fetch();
    }
    claude_code_status(&cached?)
}

/// Pure extraction from a `components.json` payload. Only the component
/// named "Claude Code" counts; every other product's status is ignored.
pub fn claude_code_status(doc: &serde_json::Value) -> Option<String> {
    let components = doc.get("components")?.as_array()?;
    let comp = components.iter().find(|c| {
        c.get("name")
            .and_then(|n| n.as_str())
            .map(|n| n.trim().to_lowercase().starts_with(COMPONENT_NAME))
            .unwrap_or(false)
    })?;
    let status = comp.get("status")?.as_str()?;
    if status == "operational" { None } else { Some(status.to_string()) }
}

fn read_cache() -> (Option<serde_json::Value>, bool) {
    let path = cache_path();
    let metadata = match fs::metadata(&path) {
        Ok(m) => m,
        Err(_) => return (None, true),
    };
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let age = SystemTime::now().duration_since(modified).unwrap_or(TTL);
    let stale = age >= TTL;

    let parsed = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());

    (parsed, stale)
}

/// Walk the scratch dir for pending tmp files dropped by previous-render
/// background curls. Validate each; promote valid ones to the cache via
/// atomic rename; unlink invalid/partial ones.
fn reconcile_pending_fetches() {
    let Ok(entries) = fs::read_dir(platform::shared_tmp_dir()) else { return };
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
        if let Ok(meta) = path.metadata()
            && let Ok(modified) = meta.modified()
            && SystemTime::now().duration_since(modified)
                .map(|d| d < Duration::from_secs(2))
                .unwrap_or(false)
        {
            continue;
        }
        match fs::read(&path) {
            Ok(bytes) if serde_json::from_slice::<serde_json::Value>(&bytes).is_ok() => {
                // Atomic POSIX rename — never produces a half-visible cache file.
                let _ = fs::rename(&path, cache_path());
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
    let tmp_full = platform::shared_tmp_dir()
        .join(format!("{}{}{}", TMP_PREFIX, std::process::id(), TMP_SUFFIX));

    let mut cmd = Command::new("curl");
    cmd.args(["-sL", "-m", "5", "-o"])
        .arg(&tmp_full)
        .arg("https://status.claude.com/api/v2/components.json")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Detached so the child outlives this render — see platform::spawn_detached.
    let _ = platform::spawn_detached(&mut cmd);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc(cc: &str, other: &str) -> serde_json::Value {
        json!({
            "status": {"indicator": "minor"},
            "components": [
                {"name": "claude.ai", "status": other},
                {"name": "Claude Code", "status": cc},
                {"name": "Claude Cowork", "status": other},
            ]
        })
    }

    #[test]
    fn other_products_degraded_does_not_fire() {
        // Page-wide indicator says "minor" but Claude Code itself is fine.
        assert_eq!(claude_code_status(&doc("operational", "major_outage")), None);
    }

    #[test]
    fn claude_code_degraded_fires_with_its_own_status() {
        assert_eq!(
            claude_code_status(&doc("partial_outage", "operational")).as_deref(),
            Some("partial_outage")
        );
    }

    #[test]
    fn missing_component_list_is_none() {
        assert_eq!(claude_code_status(&json!({"status": {"indicator": "major"}})), None);
        assert_eq!(claude_code_status(&json!({"components": []})), None);
    }

    #[test]
    fn name_match_is_case_and_whitespace_tolerant() {
        let d = json!({"components": [{"name": "  claude code (CLI) ", "status": "major_outage"}]});
        assert_eq!(claude_code_status(&d).as_deref(), Some("major_outage"));
    }
}
