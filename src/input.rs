//! Typed schema for the JSON input Claude Code sends to the statusline on
//! stdin. Every field is `Option<T>` because the schema drifts — CC versions
//! ship and drop fields without notice. A missing field is normal, never an
//! error.
//!
//! Transcript JSONL entries (read by `transcript.rs`) use `serde_json::Value`
//! instead because their shape varies too wildly to be worth modeling.

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct StatusInput {
    pub model: Option<Model>,
    pub workspace: Option<Workspace>,
    pub worktree: Option<Worktree>,
    pub pr: Option<Pr>,
    pub context_window: Option<ContextWindow>,
    pub effort: Option<Effort>,
    pub thinking: Option<Thinking>,
    pub fast_mode: Option<bool>,
    pub output_style: Option<OutputStyle>,
    pub rate_limits: Option<RateLimits>,
    pub cost: Option<Cost>,
    pub transcript_path: Option<String>,
    pub cwd: Option<String>,
    pub version: Option<String>,
    pub session_id: Option<String>,
    /// `exceeds_200k_tokens` is a TOP-LEVEL field in CC's JSON, NOT nested
    /// under context_window. Was previously misplaced inside ContextWindow,
    /// which caused the 200k+ warning to never fire (verified against a
    /// captured v2.1.145 dump where this was true but our code saw None).
    pub exceeds_200k_tokens: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Model {
    pub display_name: Option<String>,
    pub id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Workspace {
    pub current_dir: Option<String>,
    pub project_dir: Option<String>,
    pub repo: Option<WorkspaceRepo>,
    /// As of CC v2.1.145+, `workspace.git_worktree` is a STRING (the worktree
    /// name, e.g. "dump-test") for worktree sessions and absent otherwise.
    /// Earlier CC versions sent a bool. Typing this as `Option<bool>` caused
    /// the entire `Workspace` parse to fail, which (via serde's #[serde(default)]
    /// on StatusInput and the `unwrap_or_default()` in `parse_lenient`)
    /// silently defaulted EVERY field in the JSON — collapsing the statusline
    /// inside worktrees to just the model fallback and repo name.
    pub git_worktree: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WorkspaceRepo {
    pub name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Worktree {
    pub branch: Option<String>,
    pub name: Option<String>,
    pub original_branch: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Pr {
    pub number: Option<u64>,
    pub review_state: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ContextWindow {
    pub used_percentage: Option<f64>,
    pub remaining_percentage: Option<f64>,
    pub context_window_size: Option<u64>,
    pub total_tokens: Option<u64>,
    /// As of CC v2.1.132+, these report the CURRENT context window's input
    /// and output tokens — NOT cumulative session totals. The cache_read
    /// portion typically dominates total_input_tokens since most of the
    /// window is the cached prompt prefix.
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
    pub current_usage: Option<CurrentUsage>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CurrentUsage {
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Effort {
    /// One of: "max" | "xhigh" | "high" | "medium" | "low" | "min".
    /// Modeled as String not enum because CC may add new levels we don't
    /// know about yet — we'd rather render an unknown level dim than panic.
    pub level: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Thinking {
    pub enabled: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct OutputStyle {
    pub name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RateLimits {
    #[serde(deserialize_with = "lenient_window", default)]
    pub five_hour: Option<RateLimitWindow>,
    #[serde(deserialize_with = "lenient_window", default)]
    pub seven_day: Option<RateLimitWindow>,
    /// Fable 5's DEDICATED weekly limit. The wire key comes from CC's internal
    /// rate-limit state, where this window is labeled "Fable 5 limit" and fed
    /// by the `anthropic-ratelimit-unified-7d_oi-*` response headers
    /// ("overage included" — Fable usage can spill into usage credits).
    /// As of CC v2.1.201 the statusline payload builder does NOT forward it
    /// (only five_hour/seven_day) — parsed here so the segment lights up the
    /// moment CC ships it. This window is one whitelist line away in CC: it
    /// already sits in the live header-parsed state the payload is built
    /// from. See the CLAUDE.md gotcha for the verification trail.
    #[serde(deserialize_with = "lenient_window", default)]
    pub seven_day_overage_included: Option<RateLimitWindow>,
    /// Model-scoped weekly windows (CC labels "Opus limit" / "Sonnet limit").
    /// More speculative than the Fable window: CC never parses these from
    /// utilization headers — they exist only as rateLimitType enum values in
    /// 429/warning events and as fields of `GET /api/oauth/usage`. The key
    /// names here are our best guess at CC's eventual statusline naming.
    #[serde(deserialize_with = "lenient_window", default)]
    pub seven_day_opus: Option<RateLimitWindow>,
    #[serde(deserialize_with = "lenient_window", default)]
    pub seven_day_sonnet: Option<RateLimitWindow>,
}

/// Deserialize one rate-limit window, swallowing shape drift to `None`.
/// Without this, a single window arriving as a bool/number/array would fail
/// the whole `RateLimits` deserialization and `parse_lenient` would blank
/// the entire `rate_limits` slot — hiding the working 5h/7d segments too
/// (the `workspace.git_worktree` all-or-nothing bug pattern, one nesting
/// level down). Matters most for the scoped-window keys above, whose wire
/// shapes are unverified until CC actually ships them.
fn lenient_window<'de, D>(d: D) -> Result<Option<RateLimitWindow>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).ok())
}

// Serialize + Clone: usage.rs persists these into its /tmp cache in the
// same shape and hands owned copies to the render context.
#[derive(Debug, Default, Clone, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct RateLimitWindow {
    pub used_percentage: Option<f64>,
    /// `resets_at` can be a Unix-seconds number or an ISO string. We keep it
    /// as raw Value and let `format::fmt_reset_time` figure out the shape.
    pub resets_at: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Cost {
    pub total_cost_usd: Option<f64>,
    /// Total session wall-clock — includes you-typing/thinking/local work.
    pub total_duration_ms: Option<u64>,
    /// Wall-clock spent specifically waiting for Anthropic API responses
    /// (model thinking + streaming). Added in CC v2.1.132. The ratio
    /// `total_api_duration_ms / total_duration_ms` is a useful productivity
    /// signal: low = efficient use of human attention, high = lots of
    /// passive waiting on the model.
    pub total_api_duration_ms: Option<u64>,
    pub total_lines_added: Option<u64>,
    pub total_lines_removed: Option<u64>,
}

impl StatusInput {
    /// Parse from JSON bytes, field-by-field. A single drifted field
    /// (`workspace.git_worktree` once changed from bool to String, blanking
    /// the entire statusline in worktrees) only defaults its own top-level
    /// slot — all other fields survive. Net: one schema-drift incident
    /// degrades gracefully to "one segment missing" instead of "whole line
    /// empty".
    pub fn parse_lenient(buf: &[u8]) -> Self {
        if buf.is_empty() {
            return Self::default();
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(buf) else {
            return Self::default();
        };
        let Some(obj) = v.as_object() else {
            return Self::default();
        };
        Self {
            model: field(obj, "model"),
            workspace: field(obj, "workspace"),
            worktree: field(obj, "worktree"),
            pr: field(obj, "pr"),
            context_window: field(obj, "context_window"),
            effort: field(obj, "effort"),
            thinking: field(obj, "thinking"),
            fast_mode: field(obj, "fast_mode"),
            output_style: field(obj, "output_style"),
            rate_limits: field(obj, "rate_limits"),
            cost: field(obj, "cost"),
            transcript_path: field(obj, "transcript_path"),
            cwd: field(obj, "cwd"),
            version: field(obj, "version"),
            session_id: field(obj, "session_id"),
            exceeds_200k_tokens: field(obj, "exceeds_200k_tokens"),
        }
    }
}

/// Extract a single field, defaulting on missing-or-malformed. Isolates
/// schema drift to the affected slot only.
fn field<T: Default + serde::de::DeserializeOwned>(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> T {
    obj.get(key)
        .cloned()
        .and_then(|v| serde_json::from_value::<T>(v).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_defaults() {
        let s = StatusInput::parse_lenient(b"{}");
        assert!(s.model.is_none());
        assert!(s.cost.is_none());
    }

    #[test]
    fn malformed_json_doesnt_panic() {
        let s = StatusInput::parse_lenient(b"not valid json");
        assert!(s.model.is_none());
    }

    #[test]
    fn parses_realistic_input() {
        let json = br#"{
            "model": {"display_name": "Opus 4.7", "id": "claude-opus-4-7"},
            "workspace": {"current_dir": "/tmp"},
            "context_window": {"used_percentage": 42},
            "cost": {"total_cost_usd": 1.23, "total_duration_ms": 180000}
        }"#;
        let s = StatusInput::parse_lenient(json);
        assert_eq!(s.model.as_ref().unwrap().display_name.as_deref(), Some("Opus 4.7"));
        assert_eq!(s.context_window.unwrap().used_percentage, Some(42.0));
        assert_eq!(s.cost.unwrap().total_cost_usd, Some(1.23));
    }

    #[test]
    fn schema_drift_in_one_field_does_not_blank_the_others() {
        // Defensive parse: if a single top-level field drifts to an unparseable
        // shape (here: `model` becomes a bare string instead of an object),
        // only that field defaults to None — every other field survives.
        // Without field-by-field parsing, this would default the ENTIRE
        // StatusInput, hiding cost, context_window, rate_limits, etc.
        let json = br#"{
            "model": "claude-opus",
            "cost": {"total_cost_usd": 4.2},
            "context_window": {"used_percentage": 50},
            "rate_limits": {"five_hour": {"used_percentage": 30}}
        }"#;
        let s = StatusInput::parse_lenient(json);
        assert!(s.model.is_none(), "drifted field defaults to None");
        assert_eq!(s.cost.unwrap().total_cost_usd, Some(4.2));
        assert_eq!(s.context_window.unwrap().used_percentage, Some(50.0));
        assert!(s.rate_limits.is_some());
    }

    #[test]
    fn worktree_session_with_git_worktree_string_preserves_all_fields() {
        // Regression: as of CC v2.1.145+, `workspace.git_worktree` is a STRING
        // (the worktree name) for worktree sessions. Earlier CC versions sent
        // a bool. When this struct typed it as `Option<bool>`, serde failed
        // to deserialize the whole Workspace — and because parse_lenient does
        // `unwrap_or_default()` on parse failure, the entire StatusInput
        // silently collapsed to empty, hiding every metric (model, cost,
        // context_window, rate_limits) for every worktree render.
        let json = br#"{
            "model": {"display_name": "Opus 4.7", "id": "claude-opus-4-7"},
            "workspace": {
                "current_dir": "/repo/.claude/worktrees/feat-x",
                "git_worktree": "feat-x"
            },
            "worktree": {"name": "feat-x", "branch": "wt-feat-x", "original_branch": "main"},
            "context_window": {"used_percentage": 17},
            "cost": {"total_cost_usd": 4.2}
        }"#;
        let s = StatusInput::parse_lenient(json);
        assert_eq!(s.model.as_ref().unwrap().display_name.as_deref(), Some("Opus 4.7"));
        assert_eq!(s.cost.unwrap().total_cost_usd, Some(4.2));
        assert_eq!(s.context_window.unwrap().used_percentage, Some(17.0));
        assert_eq!(
            s.workspace.as_ref().unwrap().git_worktree.as_deref(),
            Some("feat-x")
        );
    }

    #[test]
    fn parses_fable_dedicated_weekly_window() {
        // seven_day_overage_included = the Fable 5 dedicated weekly limit.
        // CC doesn't forward it to the statusline yet (v2.1.201); this locks
        // in the wire key so support is live the moment it ships.
        let json = br#"{"rate_limits": {
            "five_hour": {"used_percentage": 10},
            "seven_day": {"used_percentage": 40},
            "seven_day_overage_included": {"used_percentage": 72, "resets_at": 4000000000},
            "seven_day_opus": {"used_percentage": 5},
            "seven_day_sonnet": {"used_percentage": 6}
        }}"#;
        let s = StatusInput::parse_lenient(json);
        let rl = s.rate_limits.unwrap();
        let fable = rl.seven_day_overage_included.unwrap();
        assert_eq!(fable.used_percentage, Some(72.0));
        assert!(fable.resets_at.unwrap().is_number());
        assert_eq!(rl.seven_day_opus.unwrap().used_percentage, Some(5.0));
        assert_eq!(rl.seven_day_sonnet.unwrap().used_percentage, Some(6.0));
        // The two original windows still parse alongside the new ones.
        assert_eq!(rl.five_hour.unwrap().used_percentage, Some(10.0));
        assert_eq!(rl.seven_day.unwrap().used_percentage, Some(40.0));
    }

    #[test]
    fn drifted_window_shape_nulls_only_that_window() {
        // The scoped-window wire shapes are unverified guesses until CC ships
        // them. If one arrives as a non-object (bool/string/number), only
        // that window may disappear — the working 5h/7d windows must survive.
        // (A numeric ARRAY is the one non-object shape serde still accepts:
        // derived struct deserializers map sequences positionally.)
        let json = br#"{"rate_limits": {
            "five_hour": {"used_percentage": 10},
            "seven_day": {"used_percentage": 40},
            "seven_day_overage_included": true,
            "seven_day_opus": "heavy",
            "seven_day_sonnet": 42
        }}"#;
        let s = StatusInput::parse_lenient(json);
        let rl = s.rate_limits.expect("rate_limits slot survives");
        assert_eq!(rl.five_hour.unwrap().used_percentage, Some(10.0));
        assert_eq!(rl.seven_day.unwrap().used_percentage, Some(40.0));
        assert!(rl.seven_day_overage_included.is_none());
        assert!(rl.seven_day_opus.is_none());
        assert!(rl.seven_day_sonnet.is_none());
    }

    #[test]
    fn resets_at_can_be_number_or_string() {
        let json = br#"{"rate_limits": {"five_hour": {"used_percentage": 30, "resets_at": 4000000000}}}"#;
        let s = StatusInput::parse_lenient(json);
        let resets = s.rate_limits.unwrap().five_hour.unwrap().resets_at.unwrap();
        assert!(resets.is_number());

        let json2 = br#"{"rate_limits": {"seven_day": {"resets_at": "2026-01-01T00:00:00Z"}}}"#;
        let s2 = StatusInput::parse_lenient(json2);
        let resets2 = s2.rate_limits.unwrap().seven_day.unwrap().resets_at.unwrap();
        assert!(resets2.is_string());
    }
}
