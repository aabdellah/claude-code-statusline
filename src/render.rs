//! Segment assembly + mode selection.
//!
//! Layout (full):
//!   model · repo/branch +flags+stash ↑↓ [wt←origin] wt:N #PR · todo Δ ·
//!   ctx % [gradient bar] · ⚡effort 🧠 · ◆style · 5h N% · cache N% ttl m:ss ·
//!   $X $Y/h +A/-B · Nt/s · dur
//!
//! Every segment is gated on data presence — nothing renders without a real
//! value. Red-signal events (high ctx, behind ≥3, max effort, etc.) accumulate
//! into `red_signals`; 3+ triggers a CRIT banner + red separator tint.

use std::path::Path;

use crate::ansi::{self, BOLD, CYAN, DIM, RED, RESET};
use crate::config::{self, Config, Mode};
use crate::format::{self, *};
use crate::git;
use crate::input::StatusInput;
use crate::pace;
use crate::transcript;
use crate::width;
use crate::{anthropic, ansi as ansi_mod};

pub struct RenderOutput {
    pub line: String,
    pub term_width: Option<u16>,
    pub use_compact: bool,
}

/// Collects rendered segments and the red-signal count in a single pass.
struct SegmentBag<'a> {
    full: Vec<String>,
    compact: Vec<String>,
    red_signals: u32,
    cfg: &'a Config,
}

impl<'a> SegmentBag<'a> {
    fn new(cfg: &'a Config) -> Self {
        Self {
            full: Vec::with_capacity(16),
            compact: Vec::with_capacity(16),
            red_signals: 0,
            cfg,
        }
    }

    /// Mirror of Node's `S()`. Pushes the segment to both full+compact arrays,
    /// honoring STATUSLINE_HIDE.
    fn push(&mut self, id: &str, full: String, compact: Option<String>, is_red: bool) {
        if self.cfg.is_hidden(id) { return; }
        let compact_str = compact.unwrap_or_else(|| full.clone());
        self.full.push(full);
        self.compact.push(compact_str);
        if is_red { self.red_signals += 1; }
    }
}

pub fn render(input: &StatusInput, cfg: &Config) -> RenderOutput {
    config::reset_timings();
    let mut bag = SegmentBag::new(cfg);

    // 1. Model
    bag.push(
        "model",
        format!("{}{}{}", CYAN, short_model_name(input.model.as_ref()), RESET),
        Some(format!("{}{}{}", CYAN, short_model_compact(input.model.as_ref()), RESET)),
        false,
    );

    // 2. Anthropic status (only when degraded)
    let a_status = config::timed("anthropic-status", cfg.debug_timing, anthropic::anthropic_status);
    if let Some(s) = a_status.as_deref() {
        let col = match s {
            "critical" => format!("{}{}", BOLD, RED),
            "major" => RED.to_string(),
            _ => ansi::YELLOW.to_string(),
        };
        let is_red = s != "minor";
        bag.push(
            "anthropic",
            format!("{}anthropic:{}{}", col, s, RESET),
            Some(format!("{}anth:{}{}", col, &s[..s.len().min(3)], RESET)),
            is_red,
        );
    }

    // 3. Repo / branch / git state / worktree / PR
    let cwd_str = input.workspace.as_ref().and_then(|w| w.current_dir.as_deref())
        .or(input.cwd.as_deref())
        .map(String::from)
        .unwrap_or_else(|| std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default());
    let cwd = Path::new(&cwd_str);
    let project_dir = input.workspace.as_ref().and_then(|w| w.project_dir.as_deref());
    let repo_from_schema = input.workspace.as_ref()
        .and_then(|w| w.repo.as_ref())
        .and_then(|r| r.name.as_deref());

    // Filesystem fast path: find .git/ once, read branch from HEAD directly.
    // Avoids ~25ms of git-symbolic-ref + git-rev-parse subprocess overhead.
    let gitdir = config::timed("gitdir-discover", cfg.debug_timing, || git::find_gitdir(cwd));
    let branch = input.worktree.as_ref().and_then(|w| w.branch.clone())
        .or_else(|| gitdir.as_deref().and_then(git::branch_or_sha_from_head));
    let in_repo = repo_from_schema.is_some() || gitdir.is_some();
    let in_worktree = input.worktree.as_ref().map(|w| w.name.is_some()).unwrap_or(false)
        || input.workspace.as_ref().and_then(|w| w.git_worktree).unwrap_or(false);

    // Read transcript once — yak/destruction/cache-ttl/tok-rate/ftl all use it.
    let transcript_entries = config::timed(
        "transcript-read",
        cfg.debug_timing,
        || transcript::read_transcript_tail(input.transcript_path.as_deref()),
    );

    if in_repo {
        let repo: String = repo_from_schema.map(String::from).unwrap_or_else(||
            cwd.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string()
        );

        // Parallelize the three independent git subprocesses with std::thread::scope.
        // Wall time drops from ~3× (sequential) to ~1× (max of the three).
        // todo_delta runs pre-emptively even on clean trees — its result is
        // discarded when status.dirty is false, but the wall-clock overlap
        // with status + worktree_stats means we pay nothing extra.
        let (status, wt, t_delta_pre) = config::timed("git-parallel", cfg.debug_timing, || {
            std::thread::scope(|s| {
                let h1 = s.spawn(|| git::git_status(cwd));
                let h2 = s.spawn(|| git::worktree_stats(cwd));
                let h3 = s.spawn(|| git::todo_delta(cwd));
                (h1.join().unwrap(), h2.join().unwrap(), h3.join().unwrap())
            })
        });

        let branch_str = branch.as_deref().unwrap_or("?");
        let branch_color = if branch_str == "main" || branch_str == "master" {
            if status.dirty { ansi::YELLOW } else { ansi::MAGENTA }
        } else if status.dirty { ansi::YELLOW } else { ansi::GREEN };
        let branch_seg = format!("{}{}{}", branch_color, branch_str, RESET);

        let mut full = format!("{}{}{}/{}", BOLD, repo, RESET, branch_seg);
        let mut compact = format!("{}{}{}/{}", BOLD, compact_repo_name(&repo), RESET, branch_seg);

        // Flags
        let mut flags = String::new();
        if status.staged > 0 {
            flags.push_str(&format!("{}●{}{}", ansi::GREEN, status.staged, RESET));
        }
        if status.unstaged > 0 {
            flags.push_str(&format!("{}○{}{}", ansi::YELLOW, status.unstaged, RESET));
        }
        if status.untracked > 0 {
            flags.push_str(&format!("{}+{}{}", DIM, status.untracked, RESET));
        }
        if status.stash > 0 {
            flags.push_str(&format!("{}⚑{}{}", ansi::BLUE, status.stash, RESET));
        }
        if !flags.is_empty() {
            full.push(' '); full.push_str(&flags);
            compact.push(' '); compact.push_str(&flags);
        }

        if status.ahead > 0 {
            let s = format!(" {}↑{}{}", ansi::GREEN, status.ahead, RESET);
            full.push_str(&s); compact.push_str(&s);
        }
        if status.behind > 0 {
            let s = format!(" {}↓{}{}", RED, status.behind, RESET);
            full.push_str(&s); compact.push_str(&s);
            // ≥3 behind escalates to red-signal; 1-2 behind is normal noise.
            if status.behind >= 3 { bag.red_signals += 1; }
        }

        if in_worktree {
            let origin = input.worktree.as_ref().and_then(|w| w.original_branch.as_deref());
            let origin_str = origin.map(|o| format!(" ←{}", o)).unwrap_or_default();
            full.push_str(&format!(" {}[wt{}]{}", DIM, origin_str, RESET));
        }
        if wt.extras > 0 {
            full.push_str(&format!(" {}wt:{}{}", DIM, wt.extras, RESET));
            compact.push_str(&format!(" {}wt:{}{}", DIM, wt.extras, RESET));
            if wt.stale > 0 {
                let stale_col = if wt.stale >= 5 { RED } else { ansi::YELLOW };
                full.push_str(&format!(" {}{}stale{}", stale_col, wt.stale, RESET));
                compact.push_str(&format!("{}/{}s{}", stale_col, wt.stale, RESET));
                if wt.stale >= 5 { bag.red_signals += 1; }
            }
        }

        if let Some(pr) = input.pr.as_ref() {
            if let Some(num) = pr.number {
                let pr_state = pr.review_state.as_deref();
                let pr_color = match pr_state {
                    Some("APPROVED") => ansi::GREEN,
                    Some("CHANGES_REQUESTED") => RED,
                    _ => ansi::BLUE,
                };
                let s = format!(" {}#{}{}", pr_color, num, RESET);
                full.push_str(&s); compact.push_str(&s);
                if pr_state == Some("CHANGES_REQUESTED") { bag.red_signals += 1; }
            }
        }

        bag.push("repo", full, Some(compact), false);

        // 4. Destruction counter
        let destroyed = config::timed("destruction", cfg.debug_timing,
            || transcript::destruction_count(&transcript_entries));
        if destroyed > 0 {
            let d_col: String = if destroyed >= 6 { format!("{}{}", BOLD, RED) }
                                else if destroyed >= 3 { RED.to_string() }
                                else { ansi::YELLOW.to_string() };
            bag.push(
                "destruction",
                format!("{}rm:{}{}", d_col, destroyed, RESET),
                Some(format!("{}rm{}{}", d_col, destroyed, RESET)),
                destroyed >= 3,
            );
        }

        // 5. TODO/FIXME delta — pre-computed in the parallel batch above.
        // We only USE the value when the tree is dirty; on a clean tree the
        // diff was empty and t_delta_pre is 0 anyway.
        let t_delta = if status.dirty { t_delta_pre } else { 0 };
        if t_delta != 0 {
            let sign = if t_delta > 0 {
                format!("{}+{}{}", ansi::YELLOW, t_delta, RESET)
            } else {
                format!("{}{}{}", ansi::GREEN, t_delta, RESET)
            };
            bag.push(
                "todo",
                format!("{}todo{} {}", DIM, RESET, sign),
                Some(format!("{}t{}{}", DIM, RESET, sign)),
                false,
            );
        }
    } else {
        let dir_name = cwd.file_name().and_then(|n| n.to_str()).unwrap_or("");
        bag.push("repo", format!("{}{}{}", DIM, dir_name, RESET), None, false);
    }

    // 6. cwd-drift
    if !in_worktree {
        if let Some(pd) = project_dir {
            let pd_path = Path::new(pd);
            if pd_path != cwd && !cwd.starts_with(pd_path) {
                bag.push("cwd-drift", format!("{}cwd≠proj{}", ansi::YELLOW, RESET), None, false);
            }
        }
    }

    // 7. Yak shave depth
    let yak = config::timed("yak-depth", cfg.debug_timing,
        || transcript::yak_depth(&transcript_entries));
    if let Some(yak_str) = yak_indicator(yak) {
        let yak_col = yak_color(yak);
        bag.push(
            "yak",
            format!("{}{}{}", yak_col, yak_str, RESET),
            Some(format!("{}y:{}{}", yak_col, yak, RESET)),
            yak >= 4,
        );
    }

    // 8. Context meter
    if let Some(cw) = input.context_window.as_ref() {
        let used_pct: Option<f64> = cw.used_percentage
            .or_else(|| cw.remaining_percentage.map(|r| 100.0 - r));
        if let Some(used_pct) = used_pct {
            let t = (used_pct / 100.0).clamp(0.0, 1.0) as f32;
            let bar = ansi_mod::gradient_bar(used_pct, 10);
            let size = cw.context_window_size.unwrap_or_else(|| cw.total_tokens.unwrap_or(0));
            let size_str = fmt_ctx_size(size);
            let head = format!("ctx {}%", used_pct.round() as i64);
            let mut full = format!("{} {}", ansi_mod::grad_text(&head, t), bar);
            if !size_str.is_empty() {
                full.push_str(&format!(" {}{}{}", DIM, size_str, RESET));
            }
            let exceeds = cw.exceeds_200k_tokens.unwrap_or(false);
            let compact = ansi_mod::grad_text(&compact_context_str(used_pct, size, exceeds), t);

            if exceeds {
                full.push_str(&format!(" {}{}200k+{}", RED, BOLD, RESET));
                bag.red_signals += 1;
            }
            bag.push("context", full, Some(compact), false);
            if used_pct >= 85.0 { bag.red_signals += 1; }
        }
    }

    // 9. Effort + thinking + fast (one group)
    {
        let mut full_bits: Vec<String> = Vec::new();
        let mut compact_bits: Vec<String> = Vec::new();
        let mut red = 0u32;
        if let Some(lvl) = input.effort.as_ref().and_then(|e| e.level.as_deref()) {
            let col: String = match lvl {
                "max" => format!("{}{}", BOLD, RED),
                "xhigh" => RED.to_string(),
                "high" => ansi::YELLOW.to_string(),
                "medium" => ansi::GREEN.to_string(),
                _ => DIM.to_string(),
            };
            full_bits.push(format!("{}{}{}", col, lvl, RESET));
            compact_bits.push(format!("{}{}{}", col, lvl, RESET));
            if lvl == "max" || lvl == "xhigh" { red += 1; }
        }
        if input.thinking.as_ref().and_then(|t| t.enabled).unwrap_or(false) {
            full_bits.push(format!("{}{}thinking{}", ansi::ITALIC, ansi::VIOLET, RESET));
            compact_bits.push(format!("{}{}T{}", ansi::ITALIC, ansi::VIOLET, RESET));
        }
        if input.fast_mode.unwrap_or(false) {
            full_bits.push(format!("{}{}fast{}", BOLD, ansi::BRIGHT_MAGENTA, RESET));
            compact_bits.push(format!("{}{}F{}", BOLD, ansi::BRIGHT_MAGENTA, RESET));
        }
        if !full_bits.is_empty() {
            bag.push("capabilities", full_bits.join(" "), Some(compact_bits.join("")), false);
            bag.red_signals += red;
        }
    }

    // 10. Output style (native + opt-in plugin styles)
    {
        use std::collections::BTreeSet;
        let mut styles: BTreeSet<String> = BTreeSet::new();
        if let Some(name) = input.output_style.as_ref().and_then(|s| s.name.as_deref()) {
            if name != "default" { styles.insert(name.to_string()); }
        }
        for s in active_plugin_styles(cfg) { styles.insert(s); }
        if !styles.is_empty() {
            let joined: Vec<String> = styles.iter().cloned().collect();
            let full = format!("{}{}{}", ansi::AMBER, joined.join("+"), RESET);
            let compact_joined: Vec<String> = styles
                .iter()
                .map(|s| s.chars().take(5).collect::<String>())
                .collect();
            let compact = format!("{}{}{}", ansi::AMBER, compact_joined.join("+"), RESET);
            bag.push("output-style", full, Some(compact), false);
        }
    }

    // 11. Rate limits
    {
        let mut full_bits: Vec<String> = Vec::new();
        let mut compact_bits: Vec<String> = Vec::new();
        let mut red = 0u32;
        if let Some(rl) = input.rate_limits.as_ref() {
            if let Some(fh) = rl.five_hour.as_ref() {
                if let Some(p) = fh.used_percentage {
                    let col = ansi_mod::pct_color(p, 70.0, 90.0);
                    let reset_str = fh.resets_at.as_ref()
                        .map(|v| format::fmt_reset_time(v))
                        .unwrap_or_default();
                    let mut s = format!("{}5h {}%{}", col, p.round() as i64, RESET);
                    if !reset_str.is_empty() {
                        s.push_str(&format!("{}→{}{}", DIM, reset_str, RESET));
                    }
                    full_bits.push(s);
                    compact_bits.push(format!("{}5h:{}{}", col, p.round() as i64, RESET));
                    if p >= 90.0 { red += 1; }
                }
            }
            if let Some(sd) = rl.seven_day.as_ref() {
                if let Some(pace_obj) = pace::seven_day_pace(sd) {
                    let used_col = ansi_mod::pct_color(pace_obj.used_pct, 70.0, 90.0);
                    let mut full = format!("{}7d {}%{}", used_col, pace_obj.used_pct.round() as i64, RESET);
                    let mut compact = format!("{}7d:{}{}", used_col, pace_obj.used_pct.round() as i64, RESET);
                    if let (Some(projected), Some(frac)) = (pace_obj.projected, pace_obj.frac_elapsed) {
                        let pcol = pace::pace_color(projected, frac);
                        full.push_str(&format!(" {}→{}%{}", pcol, projected.round() as i64, RESET));
                        compact.push_str(&format!("{}/{}{}", pcol, projected.round() as i64, RESET));
                        if projected > 115.0 || (projected < 80.0 && frac >= 0.70) { red += 1; }
                    }
                    if pace_obj.used_pct >= 90.0 { red += 1; }
                    full_bits.push(full);
                    compact_bits.push(compact);
                }
            }
        }
        if !full_bits.is_empty() {
            bag.push("rate-limits", full_bits.join(" "), Some(compact_bits.join(" ")), false);
            bag.red_signals += red;
        }
    }

    // 12. Cache hit % + TTL countdown
    {
        let usage = input.context_window.as_ref().and_then(|cw| cw.current_usage.as_ref());
        let cache_read = usage.and_then(|u| u.cache_read_input_tokens).unwrap_or(0);
        let cache_create = usage.and_then(|u| u.cache_creation_input_tokens).unwrap_or(0);
        let cache_total = cache_read + cache_create;
        let ttl_ms = transcript::cache_ttl_ms_remaining(&transcript_entries);
        let mut full_bits: Vec<String> = Vec::new();
        let mut compact_bits: Vec<String> = Vec::new();
        if cache_total > 0 {
            let hit_pct = (cache_read as f64 / cache_total as f64 * 100.0).round() as i64;
            let col = if hit_pct >= 80 { ansi::GREEN }
                      else if hit_pct >= 50 { ansi::YELLOW }
                      else { RED };
            full_bits.push(format!("{}cache {}%{}", col, hit_pct, RESET));
            compact_bits.push(format!("{}c:{}{}", col, hit_pct, RESET));
        }
        if let Some(ms) = ttl_ms {
            if let Some(ttl_str) = fmt_ttl(ms) {
                let ttl_color = if ms < 60_000 { RED }
                                else if ms < 180_000 { ansi::YELLOW }
                                else { ansi::GREEN };
                full_bits.push(format!("{}ttl {}{}", ttl_color, ttl_str, RESET));
                compact_bits.push(format!("{}{}{}", ttl_color, ttl_str, RESET));
            }
        }
        if !full_bits.is_empty() {
            bag.push("cache", full_bits.join(" "), Some(compact_bits.join(" ")), false);
        }
    }

    // 13. Cost + burn rate + lines + $/LOC + mileage
    //
    // Important: formatters must only be called when the source field is
    // PRESENT — not just `unwrap_or(0.0)`-defaulted. Node's `typeof x === 'number'`
    // checks distinguish missing-vs-zero; we replicate that with Option here.
    {
        let cost = input.cost.as_ref();
        let usd_opt = cost.and_then(|c| c.total_cost_usd);
        let dur_opt = cost.and_then(|c| c.total_duration_ms);
        let added = cost.and_then(|c| c.total_lines_added).unwrap_or(0);
        let removed = cost.and_then(|c| c.total_lines_removed).unwrap_or(0);
        let total_in = input.context_window.as_ref().and_then(|cw| cw.total_input_tokens).unwrap_or(0);
        let total_out = input.context_window.as_ref().and_then(|cw| cw.total_output_tokens).unwrap_or(0);

        let money = usd_opt.and_then(fmt_money);
        let money_c = usd_opt.and_then(fmt_money_compact);
        let burn = match (usd_opt, dur_opt) {
            (Some(u), Some(d)) => fmt_burn_rate(u, d),
            _ => None,
        };
        let burn_c = match (usd_opt, dur_opt) {
            (Some(u), Some(d)) => fmt_burn_rate_compact(u, d),
            _ => None,
        };
        let per_loc = usd_opt.and_then(|u| fmt_dollars_per_loc(u, added));
        let mileage = fmt_mileage(added, total_in, total_out);

        let mut full_bits: Vec<String> = Vec::new();
        let mut compact_bits: Vec<String> = Vec::new();
        if let Some(m) = money.as_ref() {
            full_bits.push(format!("{}{}{}", DIM, m, RESET));
            compact_bits.push(format!("{}{}{}", DIM, money_c.as_deref().unwrap_or(m), RESET));
        }
        if let Some(b) = burn.as_ref() {
            full_bits.push(format!("{}{}{}", DIM, b, RESET));
            compact_bits.push(format!("{}{}{}", DIM, burn_c.as_deref().unwrap_or(b), RESET));
        }
        if added > 0 || removed > 0 {
            full_bits.push(format!("{}+{}{}{}/{}{}-{}{}",
                ansi::GREEN, added, RESET, DIM, RESET, RED, removed, RESET));
            compact_bits.push(format!("{}+{}{}{}/{}{}-{}{}",
                ansi::GREEN, fmt_lines_compact(added), RESET, DIM, RESET, RED, fmt_lines_compact(removed), RESET));
        }
        // $/LOC + mileage only in full mode — nice-to-have meta.
        if let Some(p) = per_loc {
            full_bits.push(format!("{}{}{}", DIM, p, RESET));
        }
        if let Some(m) = mileage {
            full_bits.push(format!("{}{}{}", DIM, m, RESET));
        }
        if !full_bits.is_empty() {
            bag.push("cost", full_bits.join(" "), Some(compact_bits.join(" ")), false);
        }
    }

    // 14. Output tok/s + FTL approximation
    {
        let tok_rate_str = transcript::last_turn_output_rate(&transcript_entries)
            .and_then(fmt_tok_rate);
        let ftl_str = config::timed("ftl", cfg.debug_timing,
            || transcript::first_token_latency_ms(&transcript_entries))
            .and_then(fmt_ftl);
        let mut full_bits: Vec<String> = Vec::new();
        let mut compact_bits: Vec<String> = Vec::new();
        if let Some(s) = tok_rate_str.as_ref() {
            full_bits.push(format!("{}{}{}", DIM, s, RESET));
            compact_bits.push(format!("{}{}{}", DIM, s.replace("t/s", ""), RESET));
        }
        if let Some(s) = ftl_str.as_ref() {
            // Parse leading number for color decision
            let seconds: i64 = s.chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(0);
            let ftl_col = if seconds >= 10 { ansi::YELLOW } else { DIM };
            full_bits.push(format!("{}{}{}", ftl_col, s, RESET));
            compact_bits.push(format!("{}{}{}", ftl_col, s.replace("ftl ", "f"), RESET));
        }
        if !full_bits.is_empty() {
            bag.push("perf", full_bits.join(" "), Some(compact_bits.join(" ")), false);
        }
    }

    // 15. Session duration
    {
        let dur_ms = input.cost.as_ref().and_then(|c| c.total_duration_ms).unwrap_or(0);
        if let Some(dur) = fmt_duration(dur_ms) {
            let dur_c = fmt_duration_compact(dur_ms);
            bag.push("duration",
                format!("{}{}{}", DIM, dur, RESET),
                Some(format!("{}{}{}", DIM, dur_c.as_deref().unwrap_or(dur.as_str()), RESET)),
                false);
        }
    }

    // --- Mode selection + render --------------------------------------------
    let term_width = config::timed("width-detect", cfg.debug_timing, || width::detect_term_width(cfg));
    let full_sep = format!(" {}·{} ", DIM, RESET);
    let compact_sep = ' ';
    let full_line = bag.full.join(&full_sep);

    let use_compact = match cfg.mode {
        Mode::Compact => true,
        Mode::Full => false,
        Mode::Auto => {
            let visible = ansi_mod::visible_length(&full_line);
            let too_wide = term_width.map(|w| visible > w as usize).unwrap_or(false);
            let narrow = term_width.map(|w| w < cfg.compact_below).unwrap_or(false);
            too_wide || narrow
        }
    };

    let mut chosen = if use_compact {
        bag.compact.join(&compact_sep.to_string())
    } else {
        full_line
    };

    // CRIT prefix: in full mode, recolor separators red too for full-line tint;
    // in compact mode, just prepend the marker.
    if bag.red_signals >= 3 {
        let prefix = format!("{}{}CRIT{}{}", BOLD, RED, bag.red_signals, RESET);
        if use_compact {
            chosen = format!("{}{}{}", prefix, compact_sep, chosen);
        } else {
            let red_sep = format!(" {}·{} ", RED, RESET);
            chosen = chosen.replace(&full_sep, &red_sep);
            chosen = format!("{}{}{}", prefix, red_sep, chosen);
        }
    }

    RenderOutput {
        line: chosen,
        term_width,
        use_compact,
    }
}

/// Yak indicator with growing-shaggier suffix as depth increases. Catchy
/// mascot: the more nested, the more yak.
fn yak_indicator(depth: u32) -> Option<String> {
    if depth == 0 { return None; }
    let tildes = "~".repeat((depth - 1).min(5) as usize);
    Some(format!("yak{}:{}", tildes, depth))
}

fn yak_color(depth: u32) -> String {
    match depth {
        0..=1 => DIM.to_string(),
        2 => ansi::GREEN.to_string(),
        3 => ansi::YELLOW.to_string(),
        4 => RED.to_string(),
        _ => format!("{}{}", BOLD, RED),
    }
}

/// Plugin-injected output styles. Opt-in via STATUSLINE_SHOW_PLUGINS=1
/// because they're globally enabled persistent settings — surfacing them on
/// every render is signal-free noise.
fn active_plugin_styles(cfg: &Config) -> Vec<String> {
    if !cfg.show_plugins { return Vec::new(); }
    let home = std::env::var("HOME").unwrap_or_default();
    let settings_path = format!("{}/.claude/settings.json", home);
    let Ok(content) = std::fs::read_to_string(&settings_path) else { return Vec::new(); };
    let Ok(json): Result<serde_json::Value, _> = serde_json::from_str(&content) else {
        return Vec::new();
    };
    let Some(enabled) = json.get("enabledPlugins").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let mut styles = Vec::new();
    for (key, val) in enabled {
        if !val.as_bool().unwrap_or(false) { continue; }
        if key.starts_with("learning-output-style@") {
            styles.push("learning".to_string());
        } else if key.starts_with("explanatory-output-style@") {
            styles.push("explanatory".to_string());
        }
    }
    styles
}

/// Flush per-segment timings to stderr when `STATUSLINE_DEBUG_TIMING=1`.
pub fn flush_debug_timing(cfg: &Config, out: &RenderOutput) {
    if !cfg.debug_timing { return; }
    let timings = config::drain_timings();
    if timings.is_empty() { return; }
    let total: f64 = timings.iter().map(|(_, ms)| ms).sum();
    let width_info = out.term_width.map(|w| format!("{}cols", w))
        .unwrap_or_else(|| "width:unknown".into());
    let mode_info = match cfg.mode {
        Mode::Auto => format!("auto→{}", if out.use_compact { "compact" } else { "full" }),
        Mode::Full => "full".to_string(),
        Mode::Compact => "compact".to_string(),
    };
    let visible = ansi_mod::visible_length(&out.line);
    eprintln!("\n[statusline:timing] total={:.1}ms {} mode={} len={}",
              total, width_info, mode_info, visible);
    let mut sorted = timings;
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (name, ms) in &sorted {
        eprintln!("  {:>7.2}ms  {}", ms, name);
    }
}
