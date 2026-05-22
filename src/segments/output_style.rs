//! Output-style segment — CC's native output_style.name (when non-default)
//! plus optional plugin-injected styles (learning, explanatory) gated on
//! STATUSLINE_SHOW_PLUGINS=1.
//!
//! Plugin styles are opt-in because they're globally enabled persistent
//! settings — surfacing them on every render is signal-free noise unless
//! you're explicitly debugging.

use std::collections::BTreeSet;

use crate::ansi::{AMBER, RESET};
use crate::context::RenderContext;
use crate::layout::{Priority, Seg};

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let mut styles: BTreeSet<&str> = BTreeSet::new();
    if let Some(name) = ctx.input.output_style.as_ref().and_then(|s| s.name.as_deref()) {
        if name != "default" {
            styles.insert(name);
        }
    }
    for s in &ctx.plugin_styles {
        styles.insert(s.as_str());
    }

    if styles.is_empty() {
        return None;
    }

    let full_inner: String = styles.iter().copied().collect::<Vec<_>>().join("+");
    let full = format!("{}{}{}", AMBER, full_inner, RESET);
    let compact_inner: String = styles
        .iter()
        .map(|s| s.chars().take(5).collect::<String>())
        .collect::<Vec<_>>()
        .join("+");
    let compact = format!("{}{}{}", AMBER, compact_inner, RESET);

    Some(
        Seg::new("output-style", Priority::Optional, full).with_compact(compact),
    )
}
