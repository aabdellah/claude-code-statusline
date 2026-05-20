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
    pub git_worktree: Option<bool>,
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
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
    pub exceeds_200k_tokens: Option<bool>,
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
    pub five_hour: Option<RateLimitWindow>,
    pub seven_day: Option<RateLimitWindow>,
}

#[derive(Debug, Default, Deserialize)]
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
    pub total_duration_ms: Option<u64>,
    pub total_lines_added: Option<u64>,
    pub total_lines_removed: Option<u64>,
}

impl StatusInput {
    /// Parse from JSON bytes, defaulting to an empty struct on any error.
    /// Mirrors the Node version's `try { data = JSON.parse(buf || '{}') } catch {}`.
    pub fn parse_lenient(buf: &[u8]) -> Self {
        if buf.is_empty() {
            return Self::default();
        }
        serde_json::from_slice(buf).unwrap_or_default()
    }
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
