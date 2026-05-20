//! Performance segment — output tok/s + FTL (first-token latency approximation).
//!   full     "142t/s ftl 4.2s"
//!   compact  "142 f4.2s"
//!
//! Both metrics come from the most recent assistant turn in the transcript.
//! Color escalates to yellow when FTL ≥10s (suggests Anthropic queue pressure).

use crate::ansi::{DIM, RESET, YELLOW};
use crate::config;
use crate::context::RenderContext;
use crate::format::{fmt_ftl, fmt_tok_rate};
use crate::layout::{Priority, Seg};
use crate::transcript;

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    let tok_rate = transcript::last_turn_output_rate(&ctx.transcript).and_then(fmt_tok_rate);
    let ftl = config::timed("ftl", ctx.cfg.debug_timing, || {
        transcript::first_token_latency_ms(&ctx.transcript)
    })
    .and_then(fmt_ftl);

    let mut full_bits: Vec<String> = Vec::new();
    let mut compact_bits: Vec<String> = Vec::new();

    if let Some(s) = tok_rate.as_ref() {
        full_bits.push(format!("{}{}{}", DIM, s, RESET));
        compact_bits.push(format!("{}{}{}", DIM, s.replace("t/s", ""), RESET));
    }

    if let Some(s) = ftl.as_ref() {
        // Leading number is the seconds value — use it for color decision.
        let seconds: i64 = s
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .unwrap_or(0);
        let col = if seconds >= 10 { YELLOW } else { DIM };
        full_bits.push(format!("{}{}{}", col, s, RESET));
        compact_bits.push(format!("{}{}{}", col, s.replace("ftl ", "f"), RESET));
    }

    if full_bits.is_empty() {
        return None;
    }

    Some(
        Seg::new("perf", Priority::Optional, full_bits.join(" "))
            .with_compact(compact_bits.join(" ")),
    )
}
