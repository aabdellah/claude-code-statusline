//! Repo segment — "claude-code-statusline/main ●3 ↑2 #247 [wt ←main]".
//! The most information-dense segment. Builds three variants:
//!   full    repo/branch + flags + ahead/behind + worktree + PR
//!   compact same, but with shortened repo name (first hyphen segment)
//!   micro   just the branch + flags
//!
//! When NOT in a git repo, renders the cwd's last path component dim
//! as a tiny "you're here" hint.

use crate::ansi::{BLUE, BOLD, DIM, GREEN, MAGENTA, RED, RESET, YELLOW};
use crate::context::RenderContext;
use crate::format::compact_repo_name;
use crate::layout::{Priority, Seg};
use crate::repr;

pub fn render(ctx: &RenderContext) -> Option<Seg> {
    if !ctx.in_repo {
        // Fallback: dim cwd name as a minimal location hint.
        return Some(Seg::new(
            "repo",
            Priority::Important,
            format!("{}{}{}", DIM, ctx.cwd_name(), RESET),
        ));
    }

    let repo = ctx.display_repo();
    let status = &ctx.git_status;
    let wt = &ctx.worktree_stats;

    let branch_str = ctx.branch.as_deref().unwrap_or("?");
    let branch_color = if branch_str == "main" || branch_str == "master" {
        if status.dirty { YELLOW } else { MAGENTA }
    } else if status.dirty {
        YELLOW
    } else {
        GREEN
    };
    let branch_seg = format!("{}{}{}", branch_color, branch_str, RESET);

    let mut full = format!("{}{}{}/{}", BOLD, repo, RESET, branch_seg);
    let mut compact = format!("{}{}{}/{}", BOLD, compact_repo_name(&repo), RESET, branch_seg);
    let mut micro = branch_seg.clone();

    // Flags: ●N staged, ○N unstaged, +N untracked, ⚑N stashed
    let mut flags = String::new();
    if status.staged > 0 {
        flags.push_str(&format!("{}●{}{}", GREEN, status.staged, RESET));
    }
    if status.unstaged > 0 {
        flags.push_str(&format!("{}○{}{}", YELLOW, status.unstaged, RESET));
    }
    if status.untracked > 0 {
        flags.push_str(&format!("{}+{}{}", DIM, status.untracked, RESET));
    }
    if status.stash > 0 {
        flags.push_str(&format!("{}⚑{}{}", BLUE, status.stash, RESET));
    }
    if !flags.is_empty() {
        full.push(' ');
        full.push_str(&flags);
        compact.push(' ');
        compact.push_str(&flags);
        micro.push_str(&flags);
    }

    let mut red_count = 0u32;

    // Ahead / behind
    if status.ahead > 0 {
        let s = format!(" {}↑{}{}", GREEN, status.ahead, RESET);
        full.push_str(&s);
        compact.push_str(&s);
    }
    if status.behind > 0 {
        let s = format!(" {}↓{}{}", RED, status.behind, RESET);
        full.push_str(&s);
        compact.push_str(&s);
        // ≥3 behind escalates to red-signal; 1-2 behind is normal noise.
        if status.behind >= 3 {
            red_count += 1;
        }
    }

    // Worktree marker (full only — informational)
    if ctx.in_worktree {
        let origin = ctx
            .input
            .worktree
            .as_ref()
            .and_then(|w| w.original_branch.as_deref());
        let origin_str = origin.map(|o| format!(" ←{}", o)).unwrap_or_default();
        full.push_str(&format!(" {}[wt{}]{}", DIM, origin_str, RESET));
    }

    // Sibling worktrees + stale count.
    // The "wt:N" counter follows the canonical repr::counter shape.
    if wt.extras > 0 {
        let (wt_full, wt_compact) = repr::counter("wt", "wt", wt.extras, DIM);
        full.push(' ');
        full.push_str(&wt_full);
        compact.push(' ');
        compact.push_str(&wt_compact);
        if wt.stale > 0 {
            let stale_col = if wt.stale >= 5 { RED } else { YELLOW };
            full.push_str(&format!(" {}{}stale{}", stale_col, wt.stale, RESET));
            compact.push_str(&format!("{}/{}s{}", stale_col, wt.stale, RESET));
            if wt.stale >= 5 {
                red_count += 1;
            }
        }
    }

    // PR badge (from CC's JSON input)
    if let Some(pr) = ctx.input.pr.as_ref() {
        if let Some(num) = pr.number {
            let pr_state = pr.review_state.as_deref();
            let pr_color = match pr_state {
                Some("APPROVED") => GREEN,
                Some("CHANGES_REQUESTED") => RED,
                _ => BLUE,
            };
            let s = format!(" {}#{}{}", pr_color, num, RESET);
            full.push_str(&s);
            compact.push_str(&s);
            if pr_state == Some("CHANGES_REQUESTED") {
                red_count += 1;
            }
        }
    }

    let mut seg = Seg::new("repo", Priority::Important, full)
        .with_compact(compact)
        .with_micro(micro);
    if red_count > 0 {
        seg = seg.red_n(red_count);
    }
    Some(seg)
}
