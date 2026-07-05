//! Live model pricing from BerriAI/LiteLLM with embedded fallback.
//!
//! Architecture:
//!   1. `aggregate::run_refresh_today` (detached background process) calls
//!      `ensure_loaded_sync()` before scanning transcripts.
//!   2. If our cache is older than 24h AND we haven't recently failed to
//!      fetch, that synchronously curls LiteLLM's pricing JSON to a tmp
//!      file, validates it as JSON, atomic-renames to the canonical cache.
//!   3. `lookup(model)` is called per JSONL entry during scan; lazily
//!      parses the cache file once per process (`OnceLock`) and uses the
//!      resulting HashMap for O(1) lookups (exact id, then date-stripped id).
//!   4. On any lookup miss (empty cache, parse error, unknown model, or
//!      `STATUSLINE_PRICING_SOURCE=offline`), falls back to embedded
//!      Claude-family constants — partial signal beats blank cost.
//!
//! The render hot path (`read_today` → rollup cache read) NEVER touches
//! pricing. All pricing cost lives in the detached refresh process.
//!
//! Schema (per LiteLLM): each key is a model id; value carries:
//!   input_cost_per_token              ($ per 1 input token)
//!   output_cost_per_token             ($ per 1 output token)
//!   cache_creation_input_token_cost   ($ per 1 cache-write token, 5m TTL)
//!   cache_creation_input_token_cost_above_1hr  ($ per 1 cache-write, 1h)
//!   cache_read_input_token_cost       ($ per 1 cache-read token)
//!   litellm_provider                  "anthropic" filters our subset

use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

const CACHE_PATH: &str = "/tmp/cc-statusline-pricing.json";
const ATTEMPT_MARKER: &str = "/tmp/cc-statusline-pricing.attempt";
const TTL: Duration = Duration::from_secs(24 * 3600);
/// Min interval between fetch attempts. Without this, repeated failed
/// fetches (LiteLLM down, no network) would spawn one curl per render
/// refresh — at the rollup TTL of 60s that's 60 wasted attempts/hour.
const RETRY_THROTTLE: Duration = Duration::from_secs(3600);
const SOURCE_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const FETCH_TIMEOUT_SEC: u32 = 15;

#[derive(Debug, Clone, Copy)]
pub struct Pricing {
    pub input_per_m: f64,
    pub output_per_m: f64,
    pub cache_read_per_m: f64,
    pub cache_write_5m_per_m: f64,
    pub cache_write_1h_per_m: f64,
}

// Embedded Claude-family approximations (Fable 5 + Claude 4 tiers). Used
// when live pricing is unavailable, the cache is empty, the requested model
// isn't in LiteLLM, or the user has set STATUSLINE_PRICING_SOURCE=offline.
const OPUS_FALLBACK: Pricing = Pricing {
    input_per_m: 15.0,
    output_per_m: 75.0,
    cache_read_per_m: 1.50,
    cache_write_5m_per_m: 18.75,
    cache_write_1h_per_m: 30.0,
};
const SONNET_FALLBACK: Pricing = Pricing {
    input_per_m: 3.0,
    output_per_m: 15.0,
    cache_read_per_m: 0.30,
    cache_write_5m_per_m: 3.75,
    cache_write_1h_per_m: 6.0,
};
const HAIKU_FALLBACK: Pricing = Pricing {
    input_per_m: 0.80,
    output_per_m: 4.0,
    cache_read_per_m: 0.08,
    cache_write_5m_per_m: 1.00,
    cache_write_1h_per_m: 1.60,
};
// Fable 5 / Mythos 5 (same underlying model + pricing). Matches the live
// LiteLLM `claude-fable-5` entry as of 2026-07: $10/$50 per MTok.
const FABLE_FALLBACK: Pricing = Pricing {
    input_per_m: 10.0,
    output_per_m: 50.0,
    cache_read_per_m: 1.0,
    cache_write_5m_per_m: 12.5,
    cache_write_1h_per_m: 20.0,
};

#[derive(Debug, Deserialize)]
struct LiteLLMEntry {
    #[serde(default)]
    input_cost_per_token: Option<f64>,
    #[serde(default)]
    output_cost_per_token: Option<f64>,
    #[serde(default)]
    cache_creation_input_token_cost: Option<f64>,
    #[serde(default)]
    cache_creation_input_token_cost_above_1hr: Option<f64>,
    #[serde(default)]
    cache_read_input_token_cost: Option<f64>,
    #[serde(default)]
    litellm_provider: Option<String>,
}

// Process-wide parse of the cache file. Built once per process — typically
// the detached aggregate refresh process — and reused for every entry's
// lookup during the same scan.
static PARSED: OnceLock<HashMap<String, Pricing>> = OnceLock::new();

/// Synchronously refresh the pricing cache if stale. Safe to call
/// repeatedly — short-circuits when cache is fresh or we recently
/// attempted a fetch.
pub fn ensure_loaded_sync() {
    if std::env::var("STATUSLINE_PRICING_SOURCE").as_deref() == Ok("offline") {
        return;
    }
    if cache_is_fresh() {
        return;
    }
    if attempt_was_recent() {
        return;
    }
    touch_attempt_marker();
    let _ = fetch_to_cache_sync();
}

fn cache_is_fresh() -> bool {
    let Ok(metadata) = fs::metadata(CACHE_PATH) else { return false };
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let age = SystemTime::now().duration_since(modified).unwrap_or(TTL);
    age < TTL
}

fn attempt_was_recent() -> bool {
    let Ok(metadata) = fs::metadata(ATTEMPT_MARKER) else { return false };
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let age = SystemTime::now().duration_since(modified).unwrap_or(RETRY_THROTTLE);
    age < RETRY_THROTTLE
}

fn touch_attempt_marker() {
    let _ = fs::write(ATTEMPT_MARKER, b"");
}

fn fetch_to_cache_sync() -> Result<(), ()> {
    let pid = std::process::id();
    let tmp = format!("{}.{}.tmp", CACHE_PATH, pid);
    let timeout = FETCH_TIMEOUT_SEC.to_string();

    // -L follows redirects (GitHub raw → AWS CDN), -f makes curl exit
    // non-zero on 4xx/5xx, -m bounds total time.
    let status = Command::new("curl")
        .args([
            "-sLf",
            "-m", &timeout,
            "-o", &tmp,
            SOURCE_URL,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| ())?;

    if !status.success() {
        let _ = fs::remove_file(&tmp);
        return Err(());
    }

    // Validate as JSON before promoting — a partial download or HTML
    // error page would corrupt the cache otherwise.
    let bytes = fs::read(&tmp).map_err(|_| ())?;
    if serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
        let _ = fs::remove_file(&tmp);
        return Err(());
    }

    // Atomic POSIX rename — no half-visible cache file possible.
    fs::rename(&tmp, CACHE_PATH).map_err(|_| ())
}

/// Look up pricing for a model id. Tries:
///   1. Exact lowercase match in the live cache (e.g. `claude-opus-4-7-20260221`)
///   2. Date-stripped match (e.g. `claude-opus-4-7`)
///   3. Family fallback to embedded constants (substring "opus" → OPUS_FALLBACK)
///   4. `None` for non-Claude providers — caller still counts tokens but
///      skips the $ contribution.
pub fn lookup(model: &str) -> Option<Pricing> {
    let target = model.to_ascii_lowercase();

    if let Some(map) = parsed_cache() {
        if let Some(p) = map.get(&target) {
            return Some(*p);
        }
        let normalized = strip_date_suffix(&target);
        if normalized != target {
            if let Some(p) = map.get(&normalized) {
                return Some(*p);
            }
        }
    }

    if target.contains("fable") || target.contains("mythos") { return Some(FABLE_FALLBACK); }
    if target.contains("opus") { return Some(OPUS_FALLBACK); }
    if target.contains("sonnet") { return Some(SONNET_FALLBACK); }
    if target.contains("haiku") { return Some(HAIKU_FALLBACK); }
    None
}

fn parsed_cache() -> Option<&'static HashMap<String, Pricing>> {
    let map = PARSED.get_or_init(|| {
        match fs::read(CACHE_PATH) {
            Ok(bytes) => parse_litellm_json(&bytes).unwrap_or_default(),
            Err(_) => HashMap::new(),
        }
    });
    if map.is_empty() { None } else { Some(map) }
}

fn parse_litellm_json(bytes: &[u8]) -> Option<HashMap<String, Pricing>> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let obj = v.as_object()?;
    let mut out: HashMap<String, Pricing> = HashMap::with_capacity(64);
    for (key, val) in obj {
        let Ok(entry) = serde_json::from_value::<LiteLLMEntry>(val.clone()) else { continue };
        // Filter to Anthropic — LiteLLM also has bedrock/vertex duplicates
        // we don't want, and other providers entirely.
        if entry.litellm_provider.as_deref() != Some("anthropic") { continue; }
        let Some(input) = entry.input_cost_per_token else { continue };
        let Some(output) = entry.output_cost_per_token else { continue };
        let cache_read = entry.cache_read_input_token_cost.unwrap_or(0.0);
        let cache_write_5m = entry.cache_creation_input_token_cost.unwrap_or(0.0);
        // 1h cache = 2× the 5m rate (Anthropic's documented ratio) when
        // LiteLLM doesn't carry an explicit field.
        let cache_write_1h = entry.cache_creation_input_token_cost_above_1hr
            .unwrap_or(cache_write_5m * 2.0);

        let pricing = Pricing {
            input_per_m: input * 1_000_000.0,
            output_per_m: output * 1_000_000.0,
            cache_read_per_m: cache_read * 1_000_000.0,
            cache_write_5m_per_m: cache_write_5m * 1_000_000.0,
            cache_write_1h_per_m: cache_write_1h * 1_000_000.0,
        };

        let lower = key.to_ascii_lowercase();
        out.insert(lower.clone(), pricing);
        let stripped = strip_date_suffix(&lower);
        if stripped != lower {
            // First entry for a given stripped key wins — if LiteLLM has
            // multiple dated variants we keep the first encountered. The
            // exact-match path handles the actual dated id anyway.
            out.entry(stripped).or_insert(pricing);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// "claude-opus-4-7-20260221" → "claude-opus-4-7". A trailing -YYYYMMDD
/// (6+ digits after the last hyphen) gets dropped. Returns the input
/// unchanged when there's no date suffix to strip.
fn strip_date_suffix(s: &str) -> String {
    if let Some(idx) = s.rfind('-') {
        // split_at is char-boundary-safe whereas direct byte-slicing would
        // panic if `s` ever contained multi-byte chars before the hyphen.
        // `idx` is a byte offset of an ASCII '-', which IS a valid boundary,
        // so split_at always succeeds — but expressing it this way removes
        // the byte-slice (per CLAUDE.md convention on network-sourced data).
        let (prefix, after_hyphen) = s.split_at(idx);
        let suffix = &after_hyphen[1..]; // skip the '-' itself
        if suffix.len() >= 6 && suffix.chars().all(|c| c.is_ascii_digit()) {
            return prefix.to_owned();
        }
    }
    s.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_date_suffix_works() {
        assert_eq!(strip_date_suffix("claude-opus-4-20251201"), "claude-opus-4");
        assert_eq!(strip_date_suffix("claude-opus-4-7-20260221"), "claude-opus-4-7");
        // No date suffix — return unchanged.
        assert_eq!(strip_date_suffix("claude-opus-4-7"), "claude-opus-4-7");
        assert_eq!(strip_date_suffix("haiku"), "haiku");
        // Too short to be a date.
        assert_eq!(strip_date_suffix("claude-x-123"), "claude-x-123");
    }

    #[test]
    fn family_fallback_for_unknown_model_uses_embedded() {
        // Even if some prior test populated PARSED, the lookup path tries
        // exact + date-stripped first; a brand-new model id (not in any
        // LiteLLM data we'd have) hits the family fallback.
        let p = lookup("claude-opus-99-99-99999999").unwrap();
        // OPUS_FALLBACK has input_per_m = 15.0 — must be within sane bounds
        // whether we hit it directly or via LiteLLM data we may have cached.
        assert!(p.input_per_m > 5.0 && p.input_per_m < 50.0);
        assert!(p.output_per_m > p.input_per_m); // output always > input
    }

    #[test]
    fn family_fallback_covers_fable_and_mythos() {
        // "fable"/"mythos" contain no "opus"/"sonnet"/"haiku" substring —
        // without a dedicated branch these would fall to None and Fable
        // sessions would silently drop $ from the today rollup.
        let p = lookup("claude-fable-99-99999999").unwrap();
        assert!(p.input_per_m > 5.0 && p.input_per_m < 50.0);
        assert!(p.output_per_m > p.input_per_m);
        assert!(lookup("claude-mythos-99-99999999").is_some());
    }

    #[test]
    fn family_fallback_returns_none_for_non_claude() {
        // No "fable"/"mythos"/"opus"/"sonnet"/"haiku" substring → None.
        // Caller should still count tokens but skip $.
        // We can't easily assert this because if PARSED happens to be
        // populated from a previous test run, exact-match might still
        // succeed for some keys. Use a string that can't possibly match.
        assert!(lookup("xxxxxxxx-totally-unknown-provider").is_none());
    }

    #[test]
    fn parse_litellm_extracts_anthropic_entries() {
        let json = br#"{
            "claude-opus-4-7-20260221": {
                "input_cost_per_token": 0.000015,
                "output_cost_per_token": 0.000075,
                "cache_creation_input_token_cost": 0.00001875,
                "cache_read_input_token_cost": 0.0000015,
                "litellm_provider": "anthropic"
            },
            "gpt-4o": {
                "input_cost_per_token": 0.000005,
                "output_cost_per_token": 0.000015,
                "litellm_provider": "openai"
            }
        }"#;
        let map = parse_litellm_json(json).unwrap();
        assert!(map.contains_key("claude-opus-4-7-20260221"));
        assert!(map.contains_key("claude-opus-4-7")); // date-stripped
        assert!(!map.contains_key("gpt-4o")); // wrong provider
        let p = map.get("claude-opus-4-7-20260221").unwrap();
        assert!((p.input_per_m - 15.0).abs() < 0.001);
        assert!((p.output_per_m - 75.0).abs() < 0.001);
        assert!((p.cache_read_per_m - 1.5).abs() < 0.001);
        assert!((p.cache_write_5m_per_m - 18.75).abs() < 0.001);
        // 1h fallback = 2× 5m (Anthropic ratio) when LiteLLM omits the field
        assert!((p.cache_write_1h_per_m - 37.5).abs() < 0.001);
    }

    #[test]
    fn parse_litellm_uses_explicit_1h_rate_when_present() {
        let json = br#"{
            "claude-opus-4-7": {
                "input_cost_per_token": 0.000015,
                "output_cost_per_token": 0.000075,
                "cache_creation_input_token_cost": 0.00001875,
                "cache_creation_input_token_cost_above_1hr": 0.00003,
                "cache_read_input_token_cost": 0.0000015,
                "litellm_provider": "anthropic"
            }
        }"#;
        let map = parse_litellm_json(json).unwrap();
        let p = map.get("claude-opus-4-7").unwrap();
        // $30/M (explicit), not $37.5/M (2× the 5m rate fallback)
        assert!((p.cache_write_1h_per_m - 30.0).abs() < 0.001);
    }

    #[test]
    fn parse_litellm_skips_entries_missing_input_or_output_cost() {
        let json = br#"{
            "claude-broken": {
                "litellm_provider": "anthropic"
            },
            "claude-only-input": {
                "input_cost_per_token": 0.000015,
                "litellm_provider": "anthropic"
            }
        }"#;
        let result = parse_litellm_json(json);
        // No usable entries → None.
        assert!(result.is_none());
    }
}
