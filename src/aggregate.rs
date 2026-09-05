//! Today's cross-session usage aggregator.
//!
//! Per-session `cost.total_cost_usd` lives only in the live statusLine input;
//! past sessions' cost is NOT preserved in their JSONL transcripts. To roll up
//! "today's $ across all sessions" we must recompute cost ourselves from
//! `message.usage.*` token counts × an embedded pricing table.
//!
//! Day boundary is **local midnight** (per user preference). Computed by
//! `platform::local_midnight_utc_ms` — `libc::localtime_r` + `mktime` on
//! Unix, `TzSpecificLocalTimeToSystemTime` on Windows — no `chrono` dep
//! needed to keep the binary small. DST-correct either way: the conversion
//! applies the zone rules in force at that instant, which is what "today's
//! midnight" means in the user's wall clock.
//!
//! Background-refresh pattern mirrors `src/anthropic.rs`:
//!   1. Each render reads the cache file (~1ms).
//!   2. On stale/missing cache, spawn `self --refresh-today` detached.
//!   3. The detached process writes `<cache>.<pid>.tmp` and exits.
//!   4. The next render's reconcile step atomically renames any valid tmp
//!      onto the cache path. Race-free across concurrent CC sessions.
//!
//! Pricing comes from `crate::pricing` — live LiteLLM data when available,
//! Claude 4-family embedded constants as fallback. The $ figure is "what
//! API users would have paid" so subscribers can read it as an estimate
//! against their flat subscription rate. Tokens themselves are ground-truth.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use crate::format::parse_rfc3339_ms;
use crate::platform;
use crate::pricing;

// Public, machine-global data — lives in the shared scratch dir (/tmp on
// Unix, %TEMP% on Windows).
const CACHE_NAME: &str = "cc-statusline-today.json";
const TMP_PREFIX: &str = "cc-statusline-today.json.";
const TMP_SUFFIX: &str = ".tmp";
const TTL: Duration = Duration::from_secs(60);

fn cache_path() -> PathBuf {
    platform::shared_tmp_dir().join(CACHE_NAME)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TodayRollup {
    /// UTC ms of today's LOCAL midnight. Used to invalidate the cache the
    /// moment the user's wall clock rolls past midnight (even mid-render).
    pub day_anchor_ms: i64,
    /// Sum of estimated cost in USD across every assistant turn since
    /// today's local midnight. Approximate for subscribers — see module doc.
    pub cost_usd: f64,
    /// Sum of input + output + cache-write + cache-read tokens. The headline
    /// "compute used" number; matches the framing in ccusage and peers.
    pub total_tokens: u64,
}

// --- Public API -------------------------------------------------------------

/// Returns `Some(rollup)` if the cache is readable and matches today's local
/// midnight. Returns `None` if the cache is missing, malformed, or anchored
/// to a previous day (user crossed midnight since the last refresh).
///
/// Side-effect: schedules a detached background refresh on stale/missing
/// cache, so subsequent renders pick up the rolled-up data.
pub fn read_today() -> Option<TodayRollup> {
    reconcile_pending_rollups();
    let today_anchor = match platform::local_midnight_utc_ms() {
        Some(v) => v,
        None => return None,
    };
    let (cached, stale) = read_cache();
    let fresh_today = cached.as_ref().is_some_and(|r| r.day_anchor_ms == today_anchor);
    if stale || !fresh_today {
        spawn_background_refresh();
    }
    cached.filter(|r| r.day_anchor_ms == today_anchor)
}

/// Entry point for the `--refresh-today` self-invocation. Refreshes the
/// pricing cache if stale (synchronously — we're already detached), scans
/// transcripts, writes a PID-suffixed tmp file, then atomically renames it
/// onto the cache path.
///
/// The rename happens here (not on next render) because we're a synchronous
/// process and know writes are complete when we return. The reconcile step
/// in `read_today` still runs as a janitor for crashed-refresh leftovers.
pub fn run_refresh_today() {
    // Pull fresh LiteLLM pricing if our 24h cache is stale. Blocks on
    // network up to FETCH_TIMEOUT_SEC, but we're a detached background
    // process so it doesn't affect render latency.
    pricing::ensure_loaded_sync();

    // Same deal for the oauth usage fetch (Fable dedicated weekly limit —
    // not in the statusline stdin as of CC v2.1.201, see CLAUDE.md).
    crate::usage::refresh_sync();

    let Some(day_anchor) = platform::local_midnight_utc_ms() else { return };
    let rollup = scan_projects(day_anchor);
    if let Some(tmp_path) = write_tmp_file(&rollup) {
        let _ = fs::rename(&tmp_path, cache_path());
    }
}

// --- Cache I/O --------------------------------------------------------------

fn read_cache() -> (Option<TodayRollup>, bool) {
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
        .and_then(|bytes| serde_json::from_slice::<TodayRollup>(&bytes).ok());
    (parsed, stale)
}

/// Writes the rollup to a PID-suffixed tmp file. Returns the tmp path on
/// success so the caller can atomic-rename it onto the cache. Returns `None`
/// on serialization or write failure (caller silently degrades).
fn write_tmp_file(rollup: &TodayRollup) -> Option<PathBuf> {
    let tmp_full = platform::shared_tmp_dir()
        .join(format!("{}{}{}", TMP_PREFIX, std::process::id(), TMP_SUFFIX));
    let bytes = serde_json::to_vec(rollup).ok()?;
    fs::write(&tmp_full, &bytes).ok()?;
    Some(tmp_full)
}

fn reconcile_pending_rollups() {
    let Ok(entries) = fs::read_dir(platform::shared_tmp_dir()) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if !name.starts_with(TMP_PREFIX) || !name.ends_with(TMP_SUFFIX) {
            continue;
        }
        // Symlink defense — see anthropic.rs for the same rationale.
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                let _ = fs::remove_file(&path);
                continue;
            }
            Ok(_) => {}
            Err(_) => continue,
        }
        // Don't promote files that might still be mid-write.
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
            Ok(bytes) if serde_json::from_slice::<TodayRollup>(&bytes).is_ok() => {
                let _ = fs::rename(&path, cache_path());
            }
            _ => {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

fn spawn_background_refresh() {
    let Ok(exe) = std::env::current_exe() else { return };
    // Detached so the child outlives this render — see
    // platform::spawn_detached. Mirrors the pattern in src/anthropic.rs.
    let mut cmd = Command::new(exe);
    cmd.arg("--refresh-today")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _ = platform::spawn_detached(&mut cmd);
}

// --- Scan + rollup ----------------------------------------------------------

fn scan_projects(day_anchor_ms: i64) -> TodayRollup {
    let mut rollup = TodayRollup { day_anchor_ms, ..Default::default() };
    let Some(home) = platform::home_dir() else { return rollup };
    let projects_dir = home.join(".claude").join("projects");
    let Ok(project_entries) = fs::read_dir(&projects_dir) else { return rollup };
    for project in project_entries.flatten() {
        let project_path = project.path();
        if !project_path.is_dir() { continue; }
        let Ok(file_entries) = fs::read_dir(&project_path) else { continue };
        for file in file_entries.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }
            // mtime filter — skip files last touched before today's midnight.
            // Big win: most projects haven't been touched today, so we skip
            // the open+parse cost entirely.
            if let Ok(meta) = path.metadata() {
                if let Ok(modified) = meta.modified() {
                    // try_from guard — `as i64` would silently wrap to a
                    // negative value on pathological future mtimes (clock
                    // skew, malicious file metadata), causing the mtime
                    // pre-filter below to admit the file when it shouldn't.
                    let modified_ms = modified
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .ok()
                        .and_then(|d| i64::try_from(d.as_millis()).ok())
                        .unwrap_or(i64::MAX);
                    if modified_ms < day_anchor_ms { continue; }
                }
            }
            accumulate_file(&path, day_anchor_ms, &mut rollup);
        }
    }
    rollup
}

fn accumulate_file(path: &std::path::Path, day_anchor_ms: i64, rollup: &mut TodayRollup) {
    let Ok(file) = fs::File::open(path) else { return };
    let reader = BufReader::new(file);
    for line in reader.lines().map_while(Result::ok) {
        if line.is_empty() { continue; }
        if !line.contains("\"type\":\"assistant\"") { continue; }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let ts_ms = match v.get("timestamp").and_then(|t| t.as_str()).and_then(parse_rfc3339_ms) {
            Some(t) => t,
            None => continue,
        };
        if ts_ms < day_anchor_ms { continue; }
        let Some(usage) = v.get("message").and_then(|m| m.get("usage")) else { continue };
        let model = v.get("message").and_then(|m| m.get("model")).and_then(|m| m.as_str()).unwrap_or("");

        let in_tok = usage.get("input_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
        let out_tok = usage.get("output_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
        let cache_read = usage.get("cache_read_input_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
        // cache_creation.{1h,5m}_input_tokens is the breakdown; sum it if
        // present, else fall back to the flat cache_creation_input_tokens.
        let cache_write = usage
            .get("cache_creation")
            .and_then(|c| {
                let h1 = c.get("ephemeral_1h_input_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
                let m5 = c.get("ephemeral_5m_input_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
                if h1 + m5 > 0 { Some((h1, m5)) } else { None }
            })
            .unwrap_or_else(|| {
                let total = usage.get("cache_creation_input_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
                (0, total)
            });

        rollup.total_tokens = rollup.total_tokens
            .saturating_add(in_tok)
            .saturating_add(out_tok)
            .saturating_add(cache_read)
            .saturating_add(cache_write.0)
            .saturating_add(cache_write.1);

        if let Some(p) = pricing::lookup(model) {
            let m: f64 = 1_000_000.0;
            rollup.cost_usd += (in_tok as f64) * p.input_per_m / m;
            rollup.cost_usd += (out_tok as f64) * p.output_per_m / m;
            rollup.cost_usd += (cache_read as f64) * p.cache_read_per_m / m;
            rollup.cost_usd += (cache_write.0 as f64) * p.cache_write_1h_per_m / m;
            rollup.cost_usd += (cache_write.1 as f64) * p.cache_write_5m_per_m / m;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_midnight_is_aligned() {
        let ms = platform::local_midnight_utc_ms().expect("system has working TZ db");
        assert_eq!(ms % 1000, 0);
        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        // Midnight should be within the last 26 hours of "now" — covers any
        // TZ and any time of day.
        assert!((now_ms - ms) < 26 * 3600 * 1000);
        assert!((now_ms - ms) >= 0);
    }

    #[test]
    fn accumulate_file_filters_by_day_anchor() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join(format!("cc-aggregate-test-{}.jsonl", std::process::id()));
        // Use a future-dated Opus version so LiteLLM (real or test cache) is
        // guaranteed not to have an exact OR date-stripped match — pricing
        // deterministically falls back to embedded OPUS rates. Otherwise this
        // test would be sensitive to whatever's currently in the live cache.
        {
            let mut f = fs::File::create(&tmp).unwrap();
            // Before anchor — filtered out.
            writeln!(f, r#"{{"type":"assistant","timestamp":"2025-12-01T00:00:00Z","message":{{"model":"claude-opus-99-99","usage":{{"input_tokens":1000,"output_tokens":500}}}}}}"#).unwrap();
            // After anchor — counted.
            writeln!(f, r#"{{"type":"assistant","timestamp":"2026-05-21T12:00:00Z","message":{{"model":"claude-opus-99-99","usage":{{"input_tokens":2000,"output_tokens":1000}}}}}}"#).unwrap();
            // Non-assistant — ignored.
            writeln!(f, r#"{{"type":"user","timestamp":"2026-05-21T12:00:00Z"}}"#).unwrap();
        }
        // 2026-05-21T00:00:00Z = 1779321600 seconds = 1779321600000 ms
        let anchor = 1779321600000;
        let mut rollup = TodayRollup { day_anchor_ms: anchor, ..Default::default() };
        accumulate_file(&tmp, anchor, &mut rollup);
        let _ = fs::remove_file(&tmp);
        assert_eq!(rollup.total_tokens, 3000);
        // Family fallback → OPUS rates. Cost = (2000 × $15 + 1000 × $75) / 1e6
        assert!((rollup.cost_usd - 0.105).abs() < 0.001);
    }

    #[test]
    fn unknown_model_counts_tokens_but_not_cost() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join(format!("cc-aggregate-test-unknown-{}.jsonl", std::process::id()));
        {
            let mut f = fs::File::create(&tmp).unwrap();
            writeln!(f, r#"{{"type":"assistant","timestamp":"2026-05-21T12:00:00Z","message":{{"model":"gpt-4o","usage":{{"input_tokens":1000,"output_tokens":500}}}}}}"#).unwrap();
        }
        let anchor = 1779321600000;
        let mut rollup = TodayRollup { day_anchor_ms: anchor, ..Default::default() };
        accumulate_file(&tmp, anchor, &mut rollup);
        let _ = fs::remove_file(&tmp);
        assert_eq!(rollup.total_tokens, 1500);
        assert_eq!(rollup.cost_usd, 0.0);
    }

    #[test]
    fn cache_creation_breakdown_distinguishes_1h_and_5m() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join(format!("cc-aggregate-test-cache-{}.jsonl", std::process::id()));
        // Future-dated id → deterministic family fallback to OPUS embedded.
        {
            let mut f = fs::File::create(&tmp).unwrap();
            // 1M tokens 1h-cached on Opus = $30 (OPUS_FALLBACK cache_write_1h_per_m)
            writeln!(f, r#"{{"type":"assistant","timestamp":"2026-05-21T12:00:00Z","message":{{"model":"claude-opus-99-99","usage":{{"cache_creation":{{"ephemeral_1h_input_tokens":1000000,"ephemeral_5m_input_tokens":0}}}}}}}}"#).unwrap();
        }
        let anchor = 1779321600000;
        let mut rollup = TodayRollup { day_anchor_ms: anchor, ..Default::default() };
        accumulate_file(&tmp, anchor, &mut rollup);
        let _ = fs::remove_file(&tmp);
        assert!((rollup.cost_usd - 30.0).abs() < 0.01);
    }
}
