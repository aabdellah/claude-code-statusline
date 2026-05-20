//! Session-transcript JSONL reader + derived metrics.
//!
//! One tail read per render — every metric here operates on the in-memory
//! `entries` Vec produced by `read_transcript_tail()`, so we never double-I/O.
//! Entries are kept as `serde_json::Value` because the schema is heterogeneous
//! (user / assistant / sidechain / tool-result / system) and changes between
//! CC versions.

use regex::Regex;
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::OnceLock;
use std::time::SystemTime;

use crate::format::parse_rfc3339_ms;

const DEFAULT_TAIL_BYTES: u64 = 256 * 1024;
const CACHE_TTL_MS: i64 = 5 * 60 * 1000;
/// ~150 tok/s steady-state streaming. Used to approximate FTL = turn_time -
/// streaming_time. An overestimate of the stream rate makes our FTL more
/// conservative (smaller); an underestimate makes it more eager.
const ASSUMED_STREAM_RATE_TOKS_PER_SEC: f64 = 150.0;

/// Read the last `max_bytes` of the active session JSONL transcript and parse
/// each newline-delimited record. The first line is dropped when we slice
/// mid-file (it would be a partial JSON object).
pub fn read_transcript_tail(transcript_path: Option<&str>) -> Vec<Value> {
    read_transcript_tail_n(transcript_path, DEFAULT_TAIL_BYTES)
}

fn read_transcript_tail_n(transcript_path: Option<&str>, max_bytes: u64) -> Vec<Value> {
    let Some(path) = transcript_path else { return Vec::new(); };
    let path = Path::new(path);
    let Ok(mut file) = File::open(path) else { return Vec::new(); };
    let Ok(metadata) = file.metadata() else { return Vec::new(); };
    let size = metadata.len();
    let start = size.saturating_sub(max_bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::with_capacity((size - start) as usize);
    if file.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let s = String::from_utf8_lossy(&buf);
    let mut entries = Vec::new();
    let mut first = true;
    for line in s.split('\n') {
        if first && start > 0 {
            // Sliced mid-line; drop the first partial.
            first = false;
            continue;
        }
        first = false;
        if line.is_empty() { continue; }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            entries.push(v);
        }
    }
    entries
}

/// Anthropic's prompt cache window is 5 minutes, refreshed on each cache touch.
/// We approximate "time since last cache touch" by the latest entry that
/// carries a `timestamp` field (not every entry does — 'last-prompt', 'ai-title'
/// don't). Returns ms remaining (can be negative — caller decides what to do).
pub fn cache_ttl_ms_remaining(entries: &[Value]) -> Option<i64> {
    let now_ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())?;
    for e in entries.iter().rev() {
        if let Some(ts) = e.get("timestamp").and_then(|v| v.as_str()) {
            if let Some(t) = parse_rfc3339_ms(ts) {
                return Some(CACHE_TTL_MS - (now_ms - t));
            }
        }
    }
    None
}

struct LastTurn {
    out_tokens: f64,
    total_ms: f64,
}

/// Find the most recent assistant entry and the timestamped entry preceding
/// it. Returns the (output_tokens, total_ms) pair both rate metrics need.
fn find_last_assistant_turn(entries: &[Value]) -> Option<LastTurn> {
    if entries.len() < 2 { return None; }
    let last_idx = entries.iter().rposition(|e| {
        e.get("type").and_then(|v| v.as_str()) == Some("assistant")
            && e.get("timestamp").and_then(|v| v.as_str()).is_some()
    })?;
    if last_idx == 0 { return None; }

    let prev_idx = entries[..last_idx]
        .iter()
        .rposition(|e| e.get("timestamp").and_then(|v| v.as_str()).is_some())?;

    let out_tokens = entries[last_idx]
        .get("message")
        .and_then(|v| v.get("usage"))
        .and_then(|v| v.get("output_tokens"))
        .and_then(|v| v.as_f64())?;
    if out_tokens <= 0.0 { return None; }

    let t_end = parse_rfc3339_ms(entries[last_idx].get("timestamp")?.as_str()?)?;
    let t_start = parse_rfc3339_ms(entries[prev_idx].get("timestamp")?.as_str()?)?;
    let total_ms = (t_end - t_start) as f64;
    if total_ms < 0.0 { return None; }

    Some(LastTurn { out_tokens, total_ms })
}

/// "Real" Anthropic streaming throughput: output_tokens / turn-duration.
/// Bounded to [0.5s, 1h] to avoid noise and idle-time misreads.
pub fn last_turn_output_rate(entries: &[Value]) -> Option<f64> {
    let turn = find_last_assistant_turn(entries)?;
    let delta_sec = turn.total_ms / 1000.0;
    if !(0.5..=3600.0).contains(&delta_sec) { return None; }
    Some(turn.out_tokens / delta_sec)
}

/// We don't have exact first-byte timestamps; approximate as:
///   ftl ≈ total_turn_time − (output_tokens / typical_streaming_rate)
/// Honest signal of queue/processing time before streaming starts
/// (network + model loading + thinking pre-stream).
pub fn first_token_latency_ms(entries: &[Value]) -> Option<f64> {
    let turn = find_last_assistant_turn(entries)?;
    let stream_ms = (turn.out_tokens / ASSUMED_STREAM_RATE_TOKS_PER_SEC) * 1000.0;
    let ftl = turn.total_ms - stream_ms;
    Some(ftl.max(0.0))
}

/// Counts nested subagent dispatches by walking `sourceToolAssistantUUID` of
/// the most recent transcript entry. Depth 0 = main thread, 1 = inside a Task
/// subagent, 2 = subagent that called another Task, etc.
pub fn yak_depth(entries: &[Value]) -> u32 {
    if entries.is_empty() { return 0; }

    // Build uuid → entry index once
    let mut by_uuid: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        if let Some(u) = e.get("uuid").and_then(|v| v.as_str()) {
            by_uuid.insert(u, i);
        }
    }

    // Find latest entry with a sourceToolAssistantUUID link
    for e in entries.iter().rev() {
        let Some(_) = e.get("sourceToolAssistantUUID").and_then(|v| v.as_str()) else { continue; };

        let mut depth = 0u32;
        let mut current = e;
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        loop {
            let src = current.get("sourceToolAssistantUUID").and_then(|v| v.as_str());
            let Some(src) = src else { break; };
            // Only insert when we have a real UUID — using "" as a sentinel
            // would collide for ALL entries missing a uuid, causing the
            // cycle detector to break the walk early on the second such
            // entry (incorrectly capping depth).
            if let Some(cur_uuid) = current.get("uuid").and_then(|v| v.as_str()) {
                if !seen.insert(cur_uuid) { break; }
            }
            depth += 1;
            let Some(&next_idx) = by_uuid.get(src) else { break; };
            current = &entries[next_idx];
        }
        return depth;
    }
    0
}

/// Counts Bash tool invocations that look destructive (rm, unlink, truncate,
/// DROP TABLE/DATABASE/STASH, --force, --hard). "How much have I blown up this
/// session" — sleep-better-when-low signal.
pub fn destruction_count(entries: &[Value]) -> u32 {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)(\brm\s|\brm$|\bunlink\s|\btruncate\b|\bdrop\s+(?:table|database|stash)\b|--force\b|--hard\b)")
            .unwrap()
    });

    let mut count = 0u32;
    for e in entries {
        if e.get("type").and_then(|v| v.as_str()) != Some("assistant") { continue; }
        let Some(content) = e.get("message").and_then(|v| v.get("content")).and_then(|v| v.as_array()) else {
            continue;
        };
        for block in content {
            if block.get("type").and_then(|v| v.as_str()) != Some("tool_use") { continue; }
            if block.get("name").and_then(|v| v.as_str()) != Some("Bash") { continue; }
            let Some(cmd) = block.get("input").and_then(|v| v.get("command")).and_then(|v| v.as_str()) else {
                continue;
            };
            if re.is_match(cmd) { count += 1; }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn destruction_counter_matches_rm() {
        let entries = vec![
            json!({
                "type": "assistant",
                "message": {
                    "content": [
                        {"type": "tool_use", "name": "Bash", "input": {"command": "rm -rf /tmp/foo"}},
                        {"type": "tool_use", "name": "Bash", "input": {"command": "git reset --hard"}},
                        {"type": "tool_use", "name": "Bash", "input": {"command": "ls -la"}},
                    ]
                }
            })
        ];
        assert_eq!(destruction_count(&entries), 2);
    }

    #[test]
    fn yak_depth_zero_with_no_chain() {
        let entries = vec![json!({"uuid": "a", "type": "assistant"})];
        assert_eq!(yak_depth(&entries), 0);
    }

    #[test]
    fn yak_depth_one_step() {
        let entries = vec![
            json!({"uuid": "parent", "type": "assistant"}),
            json!({"uuid": "child", "type": "assistant", "sourceToolAssistantUUID": "parent"}),
        ];
        assert_eq!(yak_depth(&entries), 1);
    }

    #[test]
    fn yak_depth_two_steps() {
        let entries = vec![
            json!({"uuid": "root", "type": "assistant"}),
            json!({"uuid": "mid", "type": "assistant", "sourceToolAssistantUUID": "root"}),
            json!({"uuid": "leaf", "type": "assistant", "sourceToolAssistantUUID": "mid"}),
        ];
        assert_eq!(yak_depth(&entries), 2);
    }

    #[test]
    fn yak_depth_handles_missing_uuid_without_cycle_false_positive() {
        // Both intermediate entries have no `uuid` — the old code used `""`
        // as a sentinel which would collide on the second visit and break
        // the walk early, capping depth at 1. The fixed code only inserts
        // real UUIDs into `seen`, so the walk continues correctly.
        let entries = vec![
            json!({"uuid": "root", "type": "assistant"}),
            json!({"type": "assistant", "sourceToolAssistantUUID": "root"}),
            json!({"type": "assistant", "sourceToolAssistantUUID": "root"}),
        ];
        // The leaf points to root; walk depth = 1 (not 0 due to fake cycle).
        assert_eq!(yak_depth(&entries), 1);
    }
}
