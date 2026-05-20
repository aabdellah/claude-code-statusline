//! Output-style segment — CC's native output_style.name (when non-default)
//! plus optional plugin-injected styles (learning, explanatory) gated on
//! STATUSLINE_SHOW_PLUGINS=1.
//!
//! Plugin styles are opt-in because they're globally enabled persistent
//! settings — surfacing them on every render is signal-free noise unless
//! you're explicitly debugging.

use std::collections::BTreeSet;

use crate::ansi::{AMBER, RESET};
use crate::config::Config;
use crate::context::RenderContext;
use crate::layout::{Priority, Seg};

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let mut styles: BTreeSet<String> = BTreeSet::new();
    if let Some(name) = ctx.input.output_style.as_ref().and_then(|s| s.name.as_deref()) {
        if name != "default" {
            styles.insert(name.to_string());
        }
    }
    for s in active_plugin_styles(ctx.cfg) {
        styles.insert(s);
    }

    if styles.is_empty() {
        return None;
    }

    let joined: Vec<String> = styles.iter().cloned().collect();
    let full = format!("{}{}{}", AMBER, joined.join("+"), RESET);
    let compact_joined: Vec<String> = styles
        .iter()
        .map(|s| s.chars().take(5).collect::<String>())
        .collect();
    let compact = format!("{}{}{}", AMBER, compact_joined.join("+"), RESET);

    Some(
        Seg::new("output-style", Priority::Optional, full).with_compact(compact),
    )
}

/// Plugin-injected output styles. Opt-in via `STATUSLINE_SHOW_PLUGINS=1`.
fn active_plugin_styles(cfg: &Config) -> Vec<String> {
    if !cfg.show_plugins {
        return Vec::new();
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let settings_path = format!("{}/.claude/settings.json", home);
    let Ok(content) = std::fs::read_to_string(&settings_path) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return Vec::new();
    };
    let Some(enabled) = json.get("enabledPlugins").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let mut styles = Vec::new();
    for (key, val) in enabled {
        if !val.as_bool().unwrap_or(false) {
            continue;
        }
        if key.starts_with("learning-output-style@") {
            styles.push("learning".to_string());
        } else if key.starts_with("explanatory-output-style@") {
            styles.push("explanatory".to_string());
        }
    }
    styles
}
