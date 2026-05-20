//! Claude Code statusline — agentic-workflow tuned.
//!
//! Reads JSON on stdin per the Claude Code statusline contract and emits a
//! single rendered line to stdout. See `render.rs` for layout details and
//! `docs/ROADMAP.md` for shipped segments + backlog.

mod ansi;
mod anthropic;
mod config;
mod context;
mod format;
mod git;
mod input;
mod layout;
mod pace;
mod render;
mod repr;
mod segments;
mod transcript;
mod width;

use std::io::{self, Read, Write};

fn main() {
    let mut buf = Vec::with_capacity(4096);
    let _ = io::stdin().read_to_end(&mut buf);

    // STATUSLINE_DUMP_INPUT=1 — write the raw JSON to /tmp for inspection.
    // Used to discover new CC schema fields we're not yet consuming.
    if std::env::var("STATUSLINE_DUMP_INPUT").as_deref() == Ok("1") {
        let _ = std::fs::write("/tmp/cc-statusline-input.json", &buf);
    }

    let data = input::StatusInput::parse_lenient(&buf);
    let cfg = config::Config::from_env();
    let out = render::render(&data, &cfg);
    let _ = io::stdout().write_all(out.line.as_bytes());
    render::flush_debug_timing(&cfg, &out);
}
