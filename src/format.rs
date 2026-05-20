//! Value formatters — money, durations, rates, model names, etc.
//!
//! Every `fmt_*` helper returns `Option<String>`: `None` on junk input (NaN,
//! missing, below display threshold) so callers can simply gate rendering on
//! the result via `if let Some(s) = …`. Each formatter has a Compact variant
//! for narrow-terminal mode; both versions live side-by-side here so future
//! tweaks stay in sync.

use crate::input::Model;

// --- Money + burn rate -------------------------------------------------------

pub fn fmt_money(usd: f64) -> Option<String> {
    if !usd.is_finite() { return None; }
    if usd < 0.01 { return Some("<$0.01".into()); }
    if usd < 10.0 { return Some(format!("${:.2}", usd)); }
    Some(format!("${:.1}", usd))
}

pub fn fmt_money_compact(usd: f64) -> Option<String> {
    if !usd.is_finite() || usd <= 0.0 { return None; }
    if usd >= 10_000.0 { return Some(format!("${}k", (usd / 1000.0).round() as i64)); }
    if usd >= 1_000.0 { return Some(format!("${:.1}k", usd / 1000.0)); }
    if usd >= 10.0 { return Some(format!("${}", usd.round() as i64)); }
    if usd >= 1.0 { return Some(format!("${:.1}", usd)); }
    Some("<$1".into())
}

pub fn fmt_burn_rate(usd: f64, duration_ms: u64) -> Option<String> {
    if !usd.is_finite() { return None; }
    if duration_ms < 30_000 { return None; } // need ≥30s for stable rate
    let hours = duration_ms as f64 / 3_600_000.0;
    let rate = usd / hours;
    if !rate.is_finite() { return None; }
    if rate < 0.01 { return Some("<$0.01/h".into()); }
    if rate < 10.0 { return Some(format!("${:.2}/h", rate)); }
    Some(format!("${:.1}/h", rate))
}

pub fn fmt_burn_rate_compact(usd: f64, duration_ms: u64) -> Option<String> {
    if duration_ms < 30_000 || !usd.is_finite() { return None; }
    let rate = usd / (duration_ms as f64 / 3_600_000.0);
    if !rate.is_finite() { return None; }
    if rate >= 10.0 { return Some(format!("${}/h", rate.round() as i64)); }
    Some(format!("${:.1}/h", rate))
}

/// $ per accepted LOC. Hidden when line count is too small to be meaningful
/// (denominator noise dominates).
pub fn fmt_dollars_per_loc(usd: f64, lines_added: u64) -> Option<String> {
    if !usd.is_finite() || usd <= 0.0 { return None; }
    if lines_added < 50 { return None; }
    let per = usd / lines_added as f64;
    if per >= 1.0 { return Some(format!("${:.2}/LOC", per)); }
    Some(format!("${:.3}/LOC", per))
}

/// Lines accepted per minute of API time — productivity signal showing how
/// much code is landing for each minute the model was actually active.
///
/// This replaces the old `fmt_mileage` (LOC per 1k tokens) which became
/// mathematically wrong in CC v2.1.132+ when `total_input_tokens` /
/// `total_output_tokens` changed from cumulative session totals to current
/// context window snapshots — making the per-token denominator meaningless.
///
/// API duration (cumulative session wait-for-model time) is still cumulative,
/// so the LOC-per-minute ratio is well-defined. Renders as `lpm N`.
pub fn fmt_lines_per_api_min(lines_added: u64, api_duration_ms: u64) -> Option<String> {
    if lines_added == 0 { return None; }
    if api_duration_ms < 30_000 { return None; } // need >=30s for stability
    let minutes = api_duration_ms as f64 / 60_000.0;
    let lpm = lines_added as f64 / minutes;
    if lpm >= 100.0 { return Some(format!("lpm {}", lpm.round() as i64)); }
    if lpm >= 10.0 { return Some(format!("lpm {}", lpm.round() as i64)); }
    Some(format!("lpm {:.1}", lpm))
}

// --- Time + duration --------------------------------------------------------

pub fn fmt_duration(ms: u64) -> Option<String> {
    if ms < 1000 { return None; }
    let s = ms / 1000;
    if s < 60 { return Some(format!("{}s", s)); }
    let m = s / 60;
    if m < 60 {
        return Some(if s % 60 != 0 {
            format!("{}m{}s", m, s % 60)
        } else {
            format!("{}m", m)
        });
    }
    let h = m / 60;
    Some(format!("{}h{}m", h, m % 60))
}

pub fn fmt_duration_compact(ms: u64) -> Option<String> {
    if ms < 1000 { return None; }
    let s = ms / 1000;
    if s < 60 { return Some(format!("{}s", s)); }
    let m = s / 60;
    if m < 60 { return Some(format!("{}m", m)); }
    Some(format!("{}h", m / 60))
}

pub fn fmt_ttl(ms: i64) -> Option<String> {
    if ms <= 0 { return None; }
    let sec = ms / 1000;
    let m = sec / 60;
    let s = sec % 60;
    Some(format!("{}:{:02}", m, s))
}

/// CC sends `resets_at` as Unix seconds; older docs said ISO string. Accept
/// both shapes. Returns "" (empty string) on any parse failure / past time,
/// matching the Node version's behavior of producing a falsy value.
pub fn fmt_reset_time(secs_or_ms_or_iso: &serde_json::Value) -> String {
    let target_ms = match secs_or_ms_or_iso {
        serde_json::Value::Number(n) => {
            let v = n.as_f64().unwrap_or(0.0);
            // Heuristic: epoch seconds for ~2020-2100 are ~1.5e9-4e9; ms are ~1e12.
            if v < 1e12 { (v * 1000.0) as i64 } else { v as i64 }
        }
        serde_json::Value::String(s) => match parse_rfc3339_ms(s) {
            Some(ms) => ms,
            None => return String::new(),
        },
        _ => return String::new(),
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    let mins = ((target_ms - now_ms) as f64 / 60_000.0).round() as i64;
    if mins < 0 { return String::new(); }
    if mins < 60 { return format!("{}m", mins); }
    let h = mins / 60;
    let m = mins % 60;
    if m == 0 { format!("{}h", h) } else { format!("{}h{}m", h, m) }
}

/// Minimal RFC3339 parser — handles `YYYY-MM-DDTHH:MM:SS(.fff)?Z` and
/// `+HH:MM` / `-HH:MM` offsets. Returns Unix milliseconds. We avoid chrono
/// to keep the binary small; transcript timestamps always come in this shape.
pub(crate) fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    // YYYY-MM-DDTHH:MM:SS(.fff)?(Z|±HH:MM)
    let bytes = s.as_bytes();
    if bytes.len() < 20 { return None; }
    let year: i64 = s.get(..4)?.parse().ok()?;
    let month: i64 = s.get(5..7)?.parse().ok()?;
    let day: i64 = s.get(8..10)?.parse().ok()?;
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    let minute: i64 = s.get(14..16)?.parse().ok()?;
    let second: i64 = s.get(17..19)?.parse().ok()?;
    // Optional fractional seconds + mandatory timezone marker.
    let mut idx = 19;
    let mut millis: i64 = 0;
    if bytes.get(idx).copied() == Some(b'.') {
        idx += 1;
        let mut digits = 0;
        let mut frac = 0i64;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() && digits < 3 {
            frac = frac * 10 + (bytes[idx] - b'0') as i64;
            idx += 1;
            digits += 1;
        }
        // Skip remaining fractional digits (precision beyond ms)
        while idx < bytes.len() && bytes[idx].is_ascii_digit() { idx += 1; }
        // Pad to ms if we got fewer than 3 digits
        millis = match digits {
            0 => 0,
            1 => frac * 100,
            2 => frac * 10,
            _ => frac,
        };
    }
    // Timezone — REQUIRED per RFC 3339. We don't accept bare local times
    // because silently treating them as UTC would mis-render reset countdowns
    // by hours for non-UTC timezones.
    let tz_offset_min: i64 = match bytes.get(idx) {
        Some(b'Z') => 0,
        Some(b'+') | Some(b'-') => {
            let sign = if bytes[idx] == b'+' { 1 } else { -1 };
            let oh: i64 = s.get(idx + 1..idx + 3)?.parse().ok()?;
            let om: i64 = s.get(idx + 4..idx + 6)?.parse().ok()?;
            sign * (oh * 60 + om)
        }
        _ => return None,
    };

    // Days from civil — Howard Hinnant's algorithm, adapted to days-from-epoch.
    let y = year - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_from_epoch = era * 146097 + doe - 719468;

    let secs = days_from_epoch * 86400 + hour * 3600 + minute * 60 + second - tz_offset_min * 60;
    Some(secs * 1000 + millis)
}

// --- Tok/s + latency --------------------------------------------------------

pub fn fmt_tok_rate(rate: f64) -> Option<String> {
    if !rate.is_finite() || rate <= 0.0 { return None; }
    if rate >= 1000.0 { return Some(format!("{:.1}kt/s", rate / 1000.0)); }
    Some(format!("{}t/s", rate.round() as i64))
}

/// FTL — minimum 2s to be worth surfacing (below that, noise from our approximation).
pub fn fmt_ftl(ms: f64) -> Option<String> {
    if !ms.is_finite() || ms < 2000.0 { return None; }
    let s = ms / 1000.0;
    if s < 10.0 { return Some(format!("ftl {:.1}s", s)); }
    Some(format!("ftl {}s", s.round() as i64))
}

// --- Counts -----------------------------------------------------------------

pub fn fmt_lines_compact(n: u64) -> String {
    if n >= 10_000 { return format!("{}k", (n as f64 / 1000.0).round() as i64); }
    if n >= 1_000 { return format!("{:.1}k", n as f64 / 1000.0); }
    n.to_string()
}

// --- Context window ---------------------------------------------------------

/// 1,000,000 → "1m"; 1,500,000 → "1.5m"; 200,000 → "200k"; 1,000 → "1k".
pub fn fmt_ctx_size(tokens: u64) -> String {
    if tokens == 0 { return String::new(); }
    if tokens >= 1_000_000 {
        let m = tokens as f64 / 1_000_000.0;
        if (m - m.round()).abs() < f64::EPSILON {
            return format!("{}m", m.round() as i64);
        }
        return format!("{:.1}m", m);
    }
    format!("{}k", (tokens as f64 / 1000.0).round() as i64)
}

/// Compact context: drop the gradient bar entirely; keep "56%/1m".
pub fn compact_context_str(used_pct: f64, size: u64, exceeds_200k: bool) -> String {
    let mut out = format!("{}%", used_pct.round() as i64);
    let size_str = fmt_ctx_size(size);
    if !size_str.is_empty() {
        out.push('/');
        out.push_str(&size_str);
    }
    if exceeds_200k {
        out.push('+');
    }
    out
}

// --- Model + repo names -----------------------------------------------------

/// Prefers `model.display_name` ("Opus 4.7"); falls back to deriving from
/// `model.id` by stripping the "claude-" prefix, dropping any trailing date
/// suffix, and inserting a dot between major.minor.
pub fn short_model_name(model: Option<&Model>) -> String {
    let Some(m) = model else { return "claude".into(); };
    if let Some(d) = &m.display_name {
        // "Opus 4.7 (1M context)" → "Opus 4.7" — context size renders in the ctx segment.
        // Strip parenthetical " (... context)" suffix.
        if let Some(open) = d.rfind(" (") {
            let after = &d[open + 2..];
            if let Some(close) = after.find(')') {
                if after[..close].to_lowercase().ends_with("context") {
                    return d[..open].trim().to_string();
                }
            }
        }
        return d.trim().to_string();
    }
    let Some(id) = &m.id else { return "claude".into(); };
    let mut s = id.strip_prefix("claude-").unwrap_or(id).to_string();
    // Drop trailing date suffix like "-20251001"
    if let Some(idx) = s.rfind('-') {
        let suffix = &s[idx + 1..];
        if suffix.len() >= 6 && suffix.chars().all(|c| c.is_ascii_digit()) {
            s.truncate(idx);
        }
    }
    // Replace "-X-Y" with "-X.Y" where X and Y are numbers (e.g. -4-7 → -4.7)
    if let Some(transformed) = transform_version_dashes(&s) {
        s = transformed;
    }
    s
}

fn transform_version_dashes(s: &str) -> Option<String> {
    // Find "-N-N" (or "-N-N$") and convert to "-N.N"
    let bytes = s.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] != b'-' { continue; }
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() { j += 1; }
        if j == i + 1 || j >= bytes.len() || bytes[j] != b'-' { continue; }
        let mut k = j + 1;
        while k < bytes.len() && bytes[k].is_ascii_digit() { k += 1; }
        if k == j + 1 { continue; }
        if k != bytes.len() && bytes[k] != b'-' { continue; }
        let mut out = String::with_capacity(s.len());
        out.push_str(&s[..j]);
        out.push('.');
        out.push_str(&s[j + 1..]);
        return Some(out);
    }
    None
}

/// "Opus 4.7" → "O4.7"; "Sonnet 4.6" → "S4.6"; "haiku-4.5" → "h4.5"
pub fn short_model_compact(model: Option<&Model>) -> String {
    let name = short_model_name(model);
    let mut chars = name.chars();
    let Some(first) = chars.next() else { return name; };
    if !first.is_ascii_alphabetic() {
        return if name.chars().count() > 5 {
            name.chars().take(5).collect()
        } else {
            name
        };
    }
    // Skip remaining alphabetic chars + optional space/hyphen, keep digits onward.
    let rest: String = chars
        .skip_while(|c| c.is_ascii_alphabetic())
        .skip_while(|c| *c == ' ' || *c == '-')
        .collect();
    if rest.is_empty() {
        if name.chars().count() > 5 {
            name.chars().take(5).collect()
        } else {
            name
        }
    } else {
        format!("{}{}", first, rest)
    }
}

/// "banknet2-retail" → "banknet2"; "legacy-compat-shared" → "legacy"
/// Multi-part names: keep first hyphen/underscore segment. Single-word names:
/// truncate at 8 chars with a `~` to signal truncation.
pub fn compact_repo_name(name: &str) -> String {
    if name.is_empty() { return String::new(); }
    let first_sep = name.find(|c: char| c == '-' || c == '_');
    if let Some(idx) = first_sep {
        return name[..idx].to_string();
    }
    if name.chars().count() > 8 {
        let mut s: String = name.chars().take(7).collect();
        s.push('~');
        s
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_thresholds() {
        assert_eq!(fmt_money(0.005), Some("<$0.01".into()));
        assert_eq!(fmt_money(1.234), Some("$1.23".into()));
        assert_eq!(fmt_money(42.7), Some("$42.7".into()));
        assert_eq!(fmt_money(f64::NAN), None);
    }

    #[test]
    fn money_compact_thresholds() {
        assert_eq!(fmt_money_compact(15_000.0), Some("$15k".into()));
        assert_eq!(fmt_money_compact(1500.0), Some("$1.5k".into()));
        assert_eq!(fmt_money_compact(25.0), Some("$25".into()));
        assert_eq!(fmt_money_compact(2.5), Some("$2.5".into()));
        assert_eq!(fmt_money_compact(0.5), Some("<$1".into()));
        assert_eq!(fmt_money_compact(0.0), None);
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(fmt_duration(500), None);
        assert_eq!(fmt_duration(45_000), Some("45s".into()));
        assert_eq!(fmt_duration(180_000), Some("3m".into()));
        assert_eq!(fmt_duration(195_000), Some("3m15s".into()));
        assert_eq!(fmt_duration(3_660_000), Some("1h1m".into()));
    }

    #[test]
    fn ctx_size_formatting() {
        assert_eq!(fmt_ctx_size(0), "");
        assert_eq!(fmt_ctx_size(200_000), "200k");
        assert_eq!(fmt_ctx_size(1_000_000), "1m");
        assert_eq!(fmt_ctx_size(1_500_000), "1.5m");
    }

    #[test]
    fn model_name_strips_context_suffix() {
        let m = Model {
            display_name: Some("Opus 4.7 (1M context)".into()),
            id: None,
        };
        assert_eq!(short_model_name(Some(&m)), "Opus 4.7");
    }

    #[test]
    fn model_name_falls_back_to_id() {
        let m = Model {
            display_name: None,
            id: Some("claude-sonnet-4-6-20251001".into()),
        };
        assert_eq!(short_model_name(Some(&m)), "sonnet-4.6");
    }

    #[test]
    fn model_compact() {
        let m = Model { display_name: Some("Opus 4.7".into()), id: None };
        assert_eq!(short_model_compact(Some(&m)), "O4.7");
        let m2 = Model { display_name: Some("Sonnet 4.6".into()), id: None };
        assert_eq!(short_model_compact(Some(&m2)), "S4.6");
    }

    #[test]
    fn repo_name_compact() {
        assert_eq!(compact_repo_name("banknet2-retail"), "banknet2");
        assert_eq!(compact_repo_name("legacy_compat_shared"), "legacy");
        assert_eq!(compact_repo_name("verylongname"), "verylon~");
        assert_eq!(compact_repo_name("short"), "short");
    }

    #[test]
    fn rfc3339_parsing() {
        // 2026-01-01T00:00:00Z = 1767225600000 ms
        assert_eq!(parse_rfc3339_ms("2026-01-01T00:00:00Z"), Some(1767225600000));
        // With fractional seconds
        assert_eq!(parse_rfc3339_ms("2026-01-01T00:00:00.500Z"), Some(1767225600500));
        // With offset
        assert_eq!(parse_rfc3339_ms("2026-01-01T00:00:00+02:00"), Some(1767218400000));
    }

    #[test]
    fn rfc3339_requires_explicit_timezone() {
        // Bare time without Z or offset must return None — silently treating
        // it as UTC would mis-render reset countdowns by hours.
        assert_eq!(parse_rfc3339_ms("2026-01-01T00:00:00"), None);
    }
}
