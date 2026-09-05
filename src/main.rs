//! Claude Code statusline — agentic-workflow tuned.
//!
//! Reads JSON on stdin per the Claude Code statusline contract and emits a
//! single rendered line to stdout. See `render.rs` for layout details and
//! `docs/ROADMAP.md` for shipped segments + backlog.

mod aggregate;
mod ansi;
mod anthropic;
mod config;
mod context;
mod format;
mod git;
mod input;
mod layout;
mod pace;
mod platform;
mod pricing;
mod probe;
mod render;
mod repr;
mod segments;
mod transcript;
mod usage;
mod width;

use std::io::{self, Read, Write};

fn main() {
    // Self-invocation entry point for the background today-rollup refresh.
    // We're spawned with no stdin and `--refresh-today`; do the scan, write
    // the tmp cache file, and exit before touching the statusline render path.
    if std::env::args().any(|a| a == "--refresh-today") {
        aggregate::run_refresh_today();
        return;
    }

    // Rate-limit probe for external schedulers (no stdin contract):
    //   statusline --usage-json
    //   statusline --wait-until 'five_hour<85,seven_day<92,fable<95' [--timeout SECS]
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--usage-json") {
        std::process::exit(probe::run_usage_json());
    }
    if let Some(i) = args.iter().position(|a| a == "--wait-until") {
        let Some(spec) = args.get(i + 1) else {
            eprintln!("usage: statusline --wait-until 'five_hour<85,seven_day<92' [--timeout SECS]");
            std::process::exit(64);
        };
        let timeout = args
            .iter()
            .position(|a| a == "--timeout")
            .and_then(|t| args.get(t + 1))
            .and_then(|v| v.parse::<u64>().ok());
        std::process::exit(probe::run_wait_until(spec, timeout));
    }

    let mut buf = Vec::with_capacity(4096);
    let _ = io::stdin().read_to_end(&mut buf);

    // STATUSLINE_DUMP_INPUT=1 — write the raw JSON to the scratch dir (/tmp
    // on Unix, %TEMP% on Windows) for inspection. Used to discover new CC
    // schema fields we're not yet consuming. Path includes session_id to
    // avoid races between concurrent CC sessions all writing to the same
    // file. Also keeps overwriting the legacy path for back-compat with any
    // tooling that reads it.
    if std::env::var("STATUSLINE_DUMP_INPUT").as_deref() == Ok("1") {
        let tmp = platform::shared_tmp_dir();
        let _ = std::fs::write(tmp.join("cc-statusline-input.json"), &buf);
        let sid = serde_json::from_slice::<serde_json::Value>(&buf)
            .ok()
            .and_then(|v| v.get("session_id").and_then(|s| s.as_str()).map(String::from));
        if let Some(sid) = sid {
            let dumps = tmp.join("cc-statusline-dumps");
            let _ = std::fs::create_dir_all(&dumps);
            let _ = std::fs::write(dumps.join(format!("{}.json", sid)), &buf);
        }
    }

    let data = input::StatusInput::parse_lenient(&buf);
    let cfg = config::Config::from_env();
    let out = render::render(&data, &cfg);
    let _ = io::stdout().write_all(out.line.as_bytes());
    render::flush_debug_timing(&cfg, &out);
}
