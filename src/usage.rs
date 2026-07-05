//! Fable 5 dedicated weekly limit via `GET /api/oauth/usage`.
//!
//! CC v2.1.201 doesn't forward `seven_day_overage_included` to the
//! statusline stdin (see the CLAUDE.md gotcha), so we source the
//! model-scoped weekly windows from the same OAuth endpoint CC's own
//! `/usage` command uses. Stdin stays authoritative — the segment only
//! falls back to this data for windows stdin doesn't carry.
//!
//! Architecture (mirrors pricing.rs):
//!   1. `aggregate::run_refresh_today` (detached background process) calls
//!      `refresh_sync()`: if the cache is stale, read CC's OAuth access
//!      token, curl the endpoint, normalize, write
//!      /tmp/cc-statusline-usage.json (tmp + atomic rename).
//!   2. `read_scoped_windows()` (render hot path) reads that cache if it's
//!      recent enough; pure file read, no network, no subprocess.
//!
//! Token handling: read from `~/.claude/.credentials.json`, falling back to
//! the macOS keychain item CC uses when configured for it. The token is
//! passed to curl via a stdin config — NEVER argv (argv is visible to every
//! local process via `ps`) — and never written to disk. Its charset is
//! validated before interpolation so a tampered credentials file can't
//! inject curl config directives. `curl` and `security` are invoked by
//! absolute path so a hijacked `PATH` can't intercept the token.
//!
//! State (cache + attempt marker) lives in a PER-USER private directory
//! (`$TMPDIR`, which is `drwx------` on macOS, else `~/.cache/cc-statusline`
//! created 0700), NOT world-writable `/tmp`. The contents are
//! account-specific (this user's quota %), so — unlike pricing.rs's public
//! machine-global cache — a shared path would leak data across users, let
//! another user spoof the display, and expose the fixed paths to
//! symlink-clobber attacks. Files are written 0600 and the tmp file uses
//! `create_new` (O_EXCL) so a planted symlink is never followed.
//!
//! Opt out with `STATUSLINE_USAGE_SOURCE=off` (no fetch, no render).
//!
//! Response shape (verified live 2026-07-05 against a `max`/20x account —
//! FLAT top level, no `rate_limits` envelope):
//!   five_hour / seven_day / seven_day_opus / seven_day_sonnet (nullable):
//!     {"utilization": 0-100, "resets_at": ISO-8601}
//!   limits[]: {"kind": "weekly_scoped", "percent": 0-100, "resets_at": ISO,
//!     "scope": {"model": {"display_name": "Fable"}}}
//! The Fable dedicated weekly is the weekly_scoped entry whose model
//! display_name contains "fable" (observed "Fable"; CC matches display
//! names case-insensitively).

use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CACHE_NAME: &str = "usage.json";
const ATTEMPT_NAME: &str = "usage.attempt";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
// System binaries at fixed absolute paths — invoked directly so a hijacked
// PATH can't intercept the OAuth token (curl) or serve fake creds (security).
const CURL_BIN: &str = "/usr/bin/curl";
const SECURITY_BIN: &str = "/usr/bin/security";
/// Refresh cadence. The detached refresh runs at the today-rollup's 60s
/// TTL; this keeps usage fetches to roughly one every 2 minutes.
const TTL: Duration = Duration::from_secs(120);
/// Failed attempts (no creds, expired token, network down) don't retry
/// for this long. Cleared on the next success.
const RETRY_THROTTLE: Duration = Duration::from_secs(600);
/// Don't render data older than this — a weekly-usage % from a long-dead
/// refresh is misleading rather than helpful.
const MAX_RENDER_AGE: Duration = Duration::from_secs(15 * 60);
const FETCH_TIMEOUT_SEC: u32 = 10;

/// Model-scoped weekly windows sourced from the oauth usage endpoint,
/// normalized into stdin's `RateLimitWindow` shape so the rate_limits
/// segment can merge them under whatever CC sends directly.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScopedWindows {
    pub fable: Option<crate::input::RateLimitWindow>,
    pub opus: Option<crate::input::RateLimitWindow>,
    pub sonnet: Option<crate::input::RateLimitWindow>,
}

fn source_off() -> bool {
    std::env::var("STATUSLINE_USAGE_SOURCE").is_ok_and(|v| v == "off")
}

/// Per-user private state directory, resolved once. Prefers `$TMPDIR`
/// (macOS: `/var/folders/.../T`, mode 0700, per-user), else creates
/// `~/.cache/cc-statusline` with 0700. Returns `None` only when neither is
/// available (no HOME, no TMPDIR) — callers then no-op rather than fall back
/// to a shared world-writable path.
fn state_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            let p = PathBuf::from(tmp);
            if p.is_dir() {
                return Some(p);
            }
        }
        let home = std::env::var_os("HOME")?;
        let dir = PathBuf::from(home).join(".cache/cc-statusline");
        fs::create_dir_all(&dir).ok()?;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        Some(dir)
    })
    .as_ref()
}

fn cache_path() -> Option<PathBuf> {
    Some(state_dir()?.join(CACHE_NAME))
}

fn attempt_path() -> Option<PathBuf> {
    Some(state_dir()?.join(ATTEMPT_NAME))
}

fn age_of(path: &std::path::Path) -> Option<Duration> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    // A future mtime (backward clock step / NTP correction) means the file
    // was just written — treat as age 0 (fresh), never "not fresh".
    Some(
        SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::ZERO),
    )
}

fn fresh(path: &std::path::Path, ttl: Duration) -> bool {
    age_of(path).map(|age| age < ttl).unwrap_or(false)
}

/// Write `bytes` to `path` with mode 0600, refusing to follow a symlink
/// (`create_new` on a fresh unique tmp, then atomic rename). The tmp is
/// removed on any failure so nothing accumulates.
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Option<()> {
    use std::io::Write;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    // Best-effort clear of a stale same-pid tmp (pids recycle) so create_new
    // doesn't spuriously fail.
    let _ = fs::remove_file(&tmp);
    let result = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .ok()?;
        f.write_all(bytes).ok()?;
        drop(f);
        fs::rename(&tmp, path).ok()
    })();
    if result.is_none() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Render hot path: read the normalized cache if it's recent enough.
/// Pure file I/O — never fetches.
pub fn read_scoped_windows() -> ScopedWindows {
    if source_off() {
        return ScopedWindows::default();
    }
    let Some(path) = cache_path() else { return ScopedWindows::default() };
    if !fresh(&path, MAX_RENDER_AGE) {
        return ScopedWindows::default();
    }
    fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Detached-process path: fetch + rewrite the cache if stale. Blocks on
/// network up to FETCH_TIMEOUT_SEC — fine here, fatal on the render path.
pub fn refresh_sync() {
    if source_off() {
        return;
    }
    let (Some(cache), Some(attempt)) = (cache_path(), attempt_path()) else { return };
    if fresh(&cache, TTL) || fresh(&attempt, RETRY_THROTTLE) {
        return;
    }
    match try_refresh(&cache) {
        Some(()) => {
            let _ = fs::remove_file(&attempt);
        }
        None => {
            let _ = write_private(&attempt, b"");
        }
    }
}

fn try_refresh(cache: &std::path::Path) -> Option<()> {
    let token = load_oauth_token()?;
    let body = fetch(&token)?;
    let scoped = normalize(&body)?;
    // An account without scoped windows writes an empty cache — that
    // memoizes "nothing there" for a TTL instead of refetching each cycle.
    let bytes = serde_json::to_vec(&scoped).ok()?;
    write_private(cache, &bytes)
}

/// CC's OAuth access token: try `~/.claude/.credentials.json` first, then
/// the macOS keychain item CC uses when its "store in keychain" setting is
/// on. Each source is only accepted if it yields an unexpired token, so a
/// stale/expired credentials FILE doesn't shadow a valid KEYCHAIN token.
/// Returns None when neither source has a live token — CC rewrites the
/// credentials as it refreshes, so a later cycle picks up the new token.
fn load_oauth_token() -> Option<String> {
    credentials_file()
        .as_ref()
        .and_then(valid_token)
        .or_else(|| keychain_credentials().as_ref().and_then(valid_token))
}

/// Extract a live, well-formed access token from a parsed credentials blob.
fn valid_token(creds: &serde_json::Value) -> Option<String> {
    let oauth = creds.get("claudeAiOauth")?;
    let expires_at_ms = oauth.get("expiresAt").and_then(|v| v.as_f64())?;
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_millis() as f64;
    if expires_at_ms <= now_ms {
        return None;
    }
    let token = oauth.get("accessToken").and_then(|v| v.as_str())?;
    // Reject anything outside the JWT/base64url + separator charset before
    // it reaches the curl config, so a tampered token can't inject config
    // directives (a `"`/newline could open a `url =` / `output =` line).
    if token.is_empty()
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b))
    {
        return None;
    }
    Some(token.to_owned())
}

fn credentials_file() -> Option<serde_json::Value> {
    let home = std::env::var("HOME").ok()?;
    let bytes = fs::read(format!("{home}/.claude/.credentials.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn keychain_credentials() -> Option<serde_json::Value> {
    let out = Command::new(SECURITY_BIN)
        .args(["find-generic-password", "-s", "Claude Code-credentials", "-w"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn fetch(token: &str) -> Option<Vec<u8>> {
    use std::io::Write;
    let mut child = Command::new(CURL_BIN)
        .args([
            "-sf",
            "--max-time",
            &FETCH_TIMEOUT_SEC.to_string(),
            "--config",
            "-",
            USAGE_URL,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Headers go through a stdin config so the bearer token never appears in
    // argv. Token charset is pre-validated (see valid_token), so it can't
    // break out of the quoted directive. A write error (curl already exited)
    // is swallowed rather than early-returned so we always reap the child —
    // otherwise it lingers as a zombie for the life of the detached process.
    let cfg = format!(
        "header = \"Authorization: Bearer {token}\"\nheader = \"anthropic-beta: oauth-2025-04-20\"\n"
    );
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(cfg.as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(out.stdout)
}

/// Extract the model-scoped weekly windows from the (flat) response.
/// Returns None only on unparseable JSON — an account with no scoped
/// windows yields an empty ScopedWindows.
fn normalize(body: &[u8]) -> Option<ScopedWindows> {
    use crate::input::RateLimitWindow;
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.as_object()?;
    let mut out = ScopedWindows {
        fable: None,
        opus: top_level_window(v.get("seven_day_opus")),
        sonnet: top_level_window(v.get("seven_day_sonnet")),
    };
    if let Some(limits) = v.get("limits").and_then(|l| l.as_array()) {
        for entry in limits {
            if entry.get("kind").and_then(|k| k.as_str()) != Some("weekly_scoped") {
                continue;
            }
            let name = entry
                .pointer("/scope/model/display_name")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let win = RateLimitWindow {
                used_percentage: entry.get("percent").and_then(|p| p.as_f64()),
                resets_at: entry.get("resets_at").cloned(),
            };
            if win.used_percentage.is_none() {
                continue;
            }
            if name.contains("fable") || name.contains("mythos") {
                out.fable = Some(win);
            } else if name.contains("opus") && out.opus.is_none() {
                out.opus = Some(win);
            } else if name.contains("sonnet") && out.sonnet.is_none() {
                out.sonnet = Some(win);
            }
        }
    }
    Some(out)
}

/// Top-level windows are `{"utilization": 0-100, "resets_at": ISO}` and
/// nullable. Utilization is already a percentage (unlike the 0-1 fractions
/// in CC's response *headers*).
fn top_level_window(v: Option<&serde_json::Value>) -> Option<crate::input::RateLimitWindow> {
    let v = v?;
    let used = v.get("utilization")?.as_f64()?;
    Some(crate::input::RateLimitWindow {
        used_percentage: Some(used),
        resets_at: v.get("resets_at").cloned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Structure copied from a live 2026-07-05 response (max/20x account),
    /// trimmed to the fields we touch.
    const LIVE_SHAPE: &[u8] = br#"{
        "five_hour": {"utilization": 18.0, "resets_at": "2026-07-05T14:10:00+00:00"},
        "seven_day": {"utilization": 7.0, "resets_at": "2026-07-11T19:00:00+00:00"},
        "seven_day_opus": null,
        "seven_day_sonnet": null,
        "limits": [
            {"kind": "session", "percent": 18, "resets_at": "2026-07-05T14:10:00+00:00", "scope": null},
            {"kind": "weekly_all", "percent": 7, "resets_at": "2026-07-11T19:00:00+00:00", "scope": null},
            {"kind": "weekly_scoped", "percent": 7, "resets_at": "2026-07-11T19:00:00+00:00",
             "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null}}
        ]
    }"#;

    #[test]
    fn normalize_extracts_fable_from_weekly_scoped_limits() {
        let scoped = normalize(LIVE_SHAPE).expect("parses");
        let fable = scoped.fable.expect("fable window found");
        assert_eq!(fable.used_percentage, Some(7.0));
        assert!(fable.resets_at.unwrap().is_string());
        assert!(scoped.opus.is_none(), "null top-level window stays None");
        assert!(scoped.sonnet.is_none());
    }

    #[test]
    fn normalize_reads_top_level_scoped_windows() {
        let body = br#"{
            "seven_day_opus": {"utilization": 55.0, "resets_at": "2026-07-11T19:00:00+00:00"},
            "seven_day_sonnet": null,
            "limits": []
        }"#;
        let scoped = normalize(body).expect("parses");
        assert_eq!(scoped.opus.unwrap().used_percentage, Some(55.0));
        assert!(scoped.fable.is_none());
    }

    #[test]
    fn normalize_empty_account_yields_empty_windows() {
        let scoped = normalize(br#"{"limits": []}"#).expect("parses");
        assert!(scoped.fable.is_none() && scoped.opus.is_none() && scoped.sonnet.is_none());
    }

    #[test]
    fn normalize_rejects_non_object() {
        assert!(normalize(b"[]").is_none());
        assert!(normalize(b"not json").is_none());
    }

    #[test]
    fn scoped_windows_cache_round_trips() {
        let scoped = normalize(LIVE_SHAPE).unwrap();
        let bytes = serde_json::to_vec(&scoped).unwrap();
        let back: ScopedWindows = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.fable.unwrap().used_percentage, Some(7.0));
    }

    fn creds(token: &str, expires_at_ms: i64) -> serde_json::Value {
        serde_json::json!({
            "claudeAiOauth": {"accessToken": token, "expiresAt": expires_at_ms}
        })
    }

    fn far_future() -> i64 {
        (SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64) + 3_600_000
    }

    #[test]
    fn valid_token_accepts_jwt_charset() {
        let tok = "sk-ant-oat01_ABC.def-ghi~jkl+mno/pqr=";
        let c = creds(tok, far_future());
        assert_eq!(valid_token(&c).as_deref(), Some(tok));
    }

    #[test]
    fn valid_token_rejects_injection_chars() {
        // A double-quote or newline could break out of the curl config's
        // quoted header directive — reject before it reaches curl.
        for bad in ["x\"\nurl = \"http://evil", "tok en", "tok\\slash", "quote\"here"] {
            let c = creds(bad, far_future());
            assert!(valid_token(&c).is_none(), "must reject {bad:?}");
        }
    }

    #[test]
    fn valid_token_rejects_expired_and_missing_expiry() {
        assert!(valid_token(&creds("goodtoken", 0)).is_none(), "expired");
        // expiresAt absent → not a live token (don't send a maybe-dead token).
        let no_exp = serde_json::json!({"claudeAiOauth": {"accessToken": "goodtoken"}});
        assert!(valid_token(&no_exp).is_none(), "missing expiresAt");
    }

    #[test]
    fn valid_token_rejects_empty() {
        assert!(valid_token(&creds("", far_future())).is_none());
    }
}
