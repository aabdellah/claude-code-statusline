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
//! Token handling: BOTH stores (`~/.claude/.credentials.json` and the macOS
//! keychain item) are read and the live token with the LATER `expiresAt`
//! wins. The stores can disagree: `/login` to a different account rewrites
//! only the store CC is configured for, and the abandoned store's token
//! stays unexpired for hours — "file first" rendered the PREVIOUS account's
//! quota until that token died. CC keeps refreshing only the active store,
//! so the later expiry always identifies the active account. The token is
//! passed to curl via a stdin config — NEVER argv (argv is visible to every
//! local process via `ps`) — and never written to disk. Its charset is
//! validated before interpolation so a tampered credentials file can't
//! inject curl config directives. `curl` and `security` are invoked by
//! absolute path so a hijacked `PATH` can't intercept the token. On
//! Windows there is no keychain store — CC keeps the credentials file
//! only — and the system curl is `%SystemRoot%\System32\curl.exe`.
//!
//! State (cache + attempt marker) lives in a PER-USER private directory
//! (`$TMPDIR`, which is `drwx------` on macOS, else `~/.cache/cc-statusline`
//! created 0700; `%LOCALAPPDATA%\cc-statusline` on Windows — see
//! `platform::private_state_dir`), NOT world-writable `/tmp`. The contents
//! are account-specific (this user's quota %), so — unlike pricing.rs's
//! public machine-global cache — a shared path would leak data across
//! users, let another user spoof the display, and expose the fixed paths to
//! symlink-clobber attacks. Files are written 0600 (where the OS has mode
//! bits) and the tmp file uses `create_new` (O_EXCL) so a planted symlink
//! is never followed.
//!
//! The cache and the attempt marker are STAMPED with the active account
//! (`oauthAccount.accountUuid` from `~/.claude.json`, which CC rewrites on
//! `/login`). A stamp mismatch makes the cache unrenderable and unfresh
//! (immediate refetch as the new account) and voids the retry throttle —
//! without this, the old account's numbers kept rendering for up to
//! MAX_RENDER_AGE after a login, or RETRY_THROTTLE longer.
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
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::platform;

const CACHE_NAME: &str = "usage.json";
const ATTEMPT_NAME: &str = "usage.attempt";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
// The macOS keychain CLI at a fixed absolute path — invoked directly so a
// hijacked PATH can't serve fake creds. (curl likewise: see
// `platform::system_curl`.)
#[cfg(unix)]
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

/// On-disk cache shape: the fetched windows stamped with the account they
/// belong to. Pre-stamp cache files (bare `ScopedWindows`) deserialize with
/// `account: None` and empty windows — treated as another account's data
/// whenever the current account is known.
/// Every window `/api/oauth/usage` reports, for the `--usage-json` /
/// `--wait-until` probe (see `probe.rs`). The render path keeps using the
/// narrower `ScopedWindows` cache; this is fetched on its own cadence.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FullWindows {
    pub five_hour: Option<crate::input::RateLimitWindow>,
    pub seven_day: Option<crate::input::RateLimitWindow>,
    pub fable: Option<crate::input::RateLimitWindow>,
    pub opus: Option<crate::input::RateLimitWindow>,
    pub sonnet: Option<crate::input::RateLimitWindow>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct UsageCache {
    account: Option<String>,
    windows: ScopedWindows,
}

fn source_off() -> bool {
    std::env::var("STATUSLINE_USAGE_SOURCE").is_ok_and(|v| v == "off")
}

/// `~/.claude.json` accumulates per-project history without bound; past this
/// size, skip the account check rather than tokenize megabytes on the render
/// path. Both render and refresh use the same rule, so an over-cap file
/// degrades consistently to the unstamped (`None == None`) behavior.
const ACCOUNT_FILE_MAX: u64 = 4 * 1024 * 1024;

/// The active account per CC's own record: `oauthAccount.accountUuid` in
/// `~/.claude.json`, rewritten by CC on `/login`. `None` when the file or
/// field is unavailable — stamping then degrades to `None == None`, i.e.
/// pre-account-uuid environments keep the old unstamped behavior. Runs once
/// per render: deserialized into two thin structs (no `Value` tree of the
/// whole file) and capped at ACCOUNT_FILE_MAX.
fn current_account() -> Option<String> {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct OauthAccount {
        #[serde(rename = "accountUuid")]
        account_uuid: Option<String>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct ClaudeJson {
        #[serde(rename = "oauthAccount")]
        oauth_account: Option<OauthAccount>,
    }
    let path = platform::home_dir()?.join(".claude.json");
    if fs::metadata(&path).ok()?.len() > ACCOUNT_FILE_MAX {
        return None;
    }
    let bytes = fs::read(&path).ok()?;
    let parsed: ClaudeJson = serde_json::from_slice(&bytes).ok()?;
    parsed.oauth_account?.account_uuid
}

/// Decode a cache blob, yielding its windows only when it belongs to
/// `current`. Unparseable bytes or a foreign/legacy stamp → `None`.
fn parse_cache(bytes: &[u8], current: Option<&str>) -> Option<ScopedWindows> {
    let cache: UsageCache = serde_json::from_slice(bytes).ok()?;
    if cache.account.as_deref() != current {
        return None;
    }
    Some(cache.windows)
}

/// Per-user private state directory, resolved once — see
/// `platform::private_state_dir` for the per-OS choice. Returns `None` only
/// when nothing usable is available — callers then no-op rather than fall
/// back to a shared world-writable path.
fn state_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(platform::private_state_dir).as_ref()
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

/// Write `bytes` to `path` owner-only (0600 where the OS has mode bits),
/// refusing to follow a symlink (`create_new` on a fresh unique tmp, then
/// atomic rename). The tmp is removed on any failure so nothing accumulates.
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Option<()> {
    use std::io::Write;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    // Best-effort clear of a stale same-pid tmp (pids recycle) so create_new
    // doesn't spuriously fail.
    let _ = fs::remove_file(&tmp);
    let result = (|| {
        let mut f = platform::create_private_new(&tmp).ok()?;
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
    let current = current_account();
    fs::read(&path)
        .ok()
        .and_then(|b| parse_cache(&b, current.as_deref()))
        .unwrap_or_default()
}

/// Detached-process path: fetch + rewrite the cache if stale. Blocks on
/// network up to FETCH_TIMEOUT_SEC — fine here, fatal on the render path.
pub fn refresh_sync() {
    if source_off() {
        return;
    }
    let (Some(cache), Some(attempt)) = (cache_path(), attempt_path()) else { return };
    let current = current_account();
    if skip_refresh(
        fs::read(&cache).ok().as_deref(),
        fresh(&cache, TTL),
        fs::read(&attempt).ok().as_deref(),
        fresh(&attempt, RETRY_THROTTLE),
        current.as_deref(),
    ) {
        return;
    }
    match try_refresh(&cache, current.as_deref()) {
        Some(()) => {
            let _ = fs::remove_file(&attempt);
        }
        None => {
            let _ = write_private(&attempt, current.as_deref().unwrap_or("").as_bytes());
        }
    }
}

/// Whether the refresh can be skipped this cycle. Freshness only counts for
/// THIS account's files: after a /login the old account's cache must refetch
/// immediately, and its failed-attempt marker must not delay the new
/// account's first fetch (the marker holds the uuid of the account that
/// failed; empty = account unknown at the time).
fn skip_refresh(
    cache_bytes: Option<&[u8]>,
    cache_fresh: bool,
    attempt_bytes: Option<&[u8]>,
    attempt_fresh: bool,
    current: Option<&str>,
) -> bool {
    let cache_current = cache_bytes.is_some_and(|b| parse_cache(b, current).is_some());
    let attempt_current = attempt_bytes.is_some_and(|b| b == current.unwrap_or("").as_bytes());
    (cache_current && cache_fresh) || (attempt_current && attempt_fresh)
}

fn try_refresh(cache: &std::path::Path, account: Option<&str>) -> Option<()> {
    let token = load_oauth_token()?;
    let body = fetch(&token)?;
    let windows = normalize(&body)?;
    // An account without scoped windows writes an empty cache — that
    // memoizes "nothing there" for a TTL instead of refetching each cycle.
    let scoped = UsageCache { account: account.map(str::to_owned), windows };
    let bytes = serde_json::to_vec(&scoped).ok()?;
    write_private(cache, &bytes)
}

/// Probe road: one authenticated fetch, every window. `None` when there is
/// no live token, the network call fails, or the body is not an object.
/// Blocks up to FETCH_TIMEOUT_SEC — never call from the render path.
pub fn fetch_full() -> Option<FullWindows> {
    if source_off() {
        return None;
    }
    let token = load_oauth_token()?;
    let body = fetch(&token)?;
    normalize_full(&body)
}

/// The account stamp the probe's own cache is keyed on (same rule as the
/// render cache: a `/login` to another account invalidates it).
pub fn account_stamp() -> Option<String> {
    current_account()
}

/// Private, account-stamped state file for the probe's cache — same
/// directory and permissions as the render cache.
pub fn probe_cache_path() -> Option<PathBuf> {
    Some(state_dir()?.join("usage-full.json"))
}

pub fn write_private_file(path: &std::path::Path, bytes: &[u8]) -> Option<()> {
    write_private(path, bytes)
}

pub fn file_age(path: &std::path::Path) -> Option<Duration> {
    age_of(path)
}

/// `normalize` plus the two account-wide windows.
pub fn normalize_full(body: &[u8]) -> Option<FullWindows> {
    let scoped = normalize(body)?;
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    Some(FullWindows {
        five_hour: top_level_window(v.get("five_hour")),
        seven_day: top_level_window(v.get("seven_day")),
        fable: scoped.fable,
        opus: scoped.opus,
        sonnet: scoped.sonnet,
    })
}

/// CC's OAuth access token, from whichever store holds the ACTIVE account's
/// credentials. Returns None when neither source has a live token — CC
/// rewrites the credentials as it refreshes, so a later cycle picks up the
/// new token.
fn load_oauth_token() -> Option<String> {
    select_token(credentials_file().as_ref(), keychain_credentials().as_ref())
}

/// Pick between the credentials-file and keychain blobs: the live token with
/// the later `expiresAt` wins. A `/login` to another account rewrites only
/// the store CC is configured for; the abandoned store's token can stay
/// unexpired for hours, so "file first" served the previous account. Only CC
/// refreshes tokens, and only for the active store — later expiry ⇒ active
/// account.
fn select_token(
    file: Option<&serde_json::Value>,
    keychain: Option<&serde_json::Value>,
) -> Option<String> {
    let candidate = |creds: Option<&serde_json::Value>| -> Option<(f64, String)> {
        let creds = creds?;
        let token = valid_token(creds)?;
        let expires_at = creds.pointer("/claudeAiOauth/expiresAt")?.as_f64()?;
        Some((expires_at, token))
    };
    match (candidate(file), candidate(keychain)) {
        (Some((fe, ft)), Some((ke, kt))) => Some(if ke > fe { kt } else { ft }),
        (Some((_, t)), None) | (None, Some((_, t))) => Some(t),
        (None, None) => None,
    }
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
    let path = platform::home_dir()?.join(".claude").join(".credentials.json");
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(unix)]
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

/// No keychain store on Windows — CC writes only the credentials file.
#[cfg(not(unix))]
fn keychain_credentials() -> Option<serde_json::Value> {
    None
}

fn fetch(token: &str) -> Option<Vec<u8>> {
    use std::io::Write;
    let mut child = Command::new(platform::system_curl())
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
    fn normalize_full_carries_account_wide_windows() {
        let full = normalize_full(LIVE_SHAPE).expect("parses");
        assert_eq!(full.five_hour.unwrap().used_percentage, Some(18.0));
        assert_eq!(full.seven_day.unwrap().used_percentage, Some(7.0));
        assert_eq!(full.fable.unwrap().used_percentage, Some(7.0));
        assert!(full.opus.is_none());
        // A body with neither account-wide window still parses.
        let full = normalize_full(br#"{"limits": []}"#).expect("parses");
        assert!(full.five_hour.is_none() && full.seven_day.is_none());
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

    #[test]
    fn select_token_prefers_later_expiry_either_direction() {
        // After /login switches accounts, the store CC stopped writing keeps
        // a live token for hours — the ACTIVE account's store is the one CC
        // keeps refreshing, i.e. the later expiresAt. "File first" showed the
        // previous account's usage until its token finally expired.
        let older = creds("oldaccount", far_future());
        let newer = creds("newaccount", far_future() + 60_000);
        assert_eq!(
            select_token(Some(&older), Some(&newer)).as_deref(),
            Some("newaccount"),
            "keychain newer than file"
        );
        assert_eq!(
            select_token(Some(&newer), Some(&older)).as_deref(),
            Some("newaccount"),
            "file newer than keychain"
        );
    }

    #[test]
    fn select_token_single_source_and_none() {
        let only = creds("solo", far_future());
        assert_eq!(select_token(Some(&only), None).as_deref(), Some("solo"));
        assert_eq!(select_token(None, Some(&only)).as_deref(), Some("solo"));
        assert!(select_token(None, None).is_none());
    }

    #[test]
    fn select_token_skips_dead_source_regardless_of_expiry_ordering() {
        // An expired token never wins, even with the later expiresAt absent
        // or the live token's expiry being "earlier" than a bogus future one
        // paired with an invalid charset.
        let expired = creds("deadtoken", 0);
        let live = creds("livetoken", far_future());
        assert_eq!(select_token(Some(&expired), Some(&live)).as_deref(), Some("livetoken"));
        let bad_charset = creds("to ken\"", far_future() + 999_000);
        assert_eq!(select_token(Some(&bad_charset), Some(&live)).as_deref(), Some("livetoken"));
    }

    #[test]
    fn parse_cache_rejects_other_accounts_data() {
        let cache = UsageCache {
            account: Some("uuid-old".into()),
            windows: normalize(LIVE_SHAPE).unwrap(),
        };
        let bytes = serde_json::to_vec(&cache).unwrap();
        assert!(parse_cache(&bytes, Some("uuid-new")).is_none(), "other account");
        assert!(parse_cache(&bytes, None).is_none(), "account no longer known");
        let windows = parse_cache(&bytes, Some("uuid-old")).expect("same account renders");
        assert_eq!(windows.fable.unwrap().used_percentage, Some(7.0));
    }

    #[test]
    fn parse_cache_unstamped_matches_only_unknown_account() {
        // account: None (no ~/.claude.json) round-trips against None.
        let cache = UsageCache { account: None, windows: normalize(LIVE_SHAPE).unwrap() };
        let bytes = serde_json::to_vec(&cache).unwrap();
        assert!(parse_cache(&bytes, None).is_some());
        assert!(parse_cache(&bytes, Some("uuid-new")).is_none());
    }

    #[test]
    fn parse_cache_legacy_format_never_renders_under_known_account() {
        // Pre-stamp cache files are a bare ScopedWindows object — after an
        // account switch they hold the OLD account's numbers, so a known
        // current account must discard them (one-time refetch on upgrade).
        let legacy = serde_json::to_vec(&normalize(LIVE_SHAPE).unwrap()).unwrap();
        assert!(parse_cache(&legacy, Some("uuid-new")).is_none());
    }

    fn stamped_cache(account: Option<&str>) -> Vec<u8> {
        let cache = UsageCache {
            account: account.map(str::to_owned),
            windows: normalize(LIVE_SHAPE).unwrap(),
        };
        serde_json::to_vec(&cache).unwrap()
    }

    #[test]
    fn skip_refresh_honors_only_this_accounts_fresh_state() {
        let mine = stamped_cache(Some("me"));
        let theirs = stamped_cache(Some("them"));
        // Fresh cache: skippable only when it's the current account's.
        assert!(skip_refresh(Some(&mine), true, None, false, Some("me")));
        assert!(!skip_refresh(Some(&theirs), true, None, false, Some("me")), "post-/login refetch");
        assert!(!skip_refresh(Some(&mine), false, None, false, Some("me")), "stale cache refetches");
        // Legacy unstamped cache never counts as fresh under a known account.
        let legacy = serde_json::to_vec(&normalize(LIVE_SHAPE).unwrap()).unwrap();
        assert!(!skip_refresh(Some(&legacy), true, None, false, Some("me")));
    }

    #[test]
    fn skip_refresh_throttles_failures_per_account() {
        // A fresh failed-attempt marker throttles only the account that
        // failed; a /login must not inherit the old account's back-off.
        assert!(skip_refresh(None, false, Some(b"me"), true, Some("me")));
        assert!(!skip_refresh(None, false, Some(b"them"), true, Some("me")), "new login retries now");
        assert!(!skip_refresh(None, false, Some(b"me"), false, Some("me")), "stale marker retries");
        // Unknown account (no ~/.claude.json): empty marker matches — the
        // pre-stamp behavior.
        assert!(skip_refresh(None, false, Some(b""), true, None));
        assert!(!skip_refresh(None, false, Some(b"them"), true, None));
    }
}
