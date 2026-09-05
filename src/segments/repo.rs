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
    if let Some(pr) = ctx.input.pr.as_ref()
        && let Some(num) = pr.number
    {
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

    let mut seg = Seg::new("repo", Priority::Important, full)
        .with_compact(compact)
        .with_micro(micro);
    if red_count > 0 {
        seg = seg.red_n(red_count);
    }
    Some(seg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::strip_ansi;
    use crate::config::Config;
    use crate::context::RenderContext;
    use crate::git::WorktreeStats;
    use crate::input::{Pr, StatusInput};

    fn ctx_with_pr<'a>(
        input: &'a StatusInput,
        cfg: &'a Config,
        branch: &str,
        worktree_stats: WorktreeStats,
    ) -> RenderContext<'a> {
        let mut ctx = RenderContext::test_default(input, cfg);
        ctx.in_repo = true;
        ctx.branch = Some(branch.to_owned());
        ctx.repo_name = Some("demo-repo".to_owned());
        ctx.worktree_stats = worktree_stats;
        ctx
    }

    fn input_with_pr(num: u64, review_state: Option<&str>) -> StatusInput {
        StatusInput {
            pr: Some(Pr {
                number: Some(num),
                review_state: review_state.map(str::to_owned),
            }),
            ..StatusInput::default()
        }
    }

    #[test]
    fn pr_changes_requested_contributes_red_signal() {
        let input = input_with_pr(42, Some("CHANGES_REQUESTED"));
        let cfg = Config::from_env();
        let ctx = ctx_with_pr(&input, &cfg, "feat-x", WorktreeStats::default());
        let seg = render(&ctx).expect("renders");
        assert_eq!(seg.red_count, 1, "CHANGES_REQUESTED is a red signal");
        assert!(strip_ansi(&seg.full).contains("#42"));
    }

    #[test]
    fn pr_approved_no_red_signal() {
        let input = input_with_pr(42, Some("APPROVED"));
        let cfg = Config::from_env();
        let ctx = ctx_with_pr(&input, &cfg, "feat-x", WorktreeStats::default());
        let seg = render(&ctx).expect("renders");
        assert_eq!(seg.red_count, 0);
    }

    #[test]
    fn five_or_more_stale_worktrees_red() {
        let input = StatusInput::default();
        let cfg = Config::from_env();
        let stats = WorktreeStats { extras: 5, stale: 5 };
        let ctx = ctx_with_pr(&input, &cfg, "main", stats);
        let seg = render(&ctx).expect("renders");
        assert_eq!(seg.red_count, 1, "≥5 stale worktrees is a red signal");
    }

    #[test]
    fn fewer_than_five_stale_no_red() {
        let input = StatusInput::default();
        let cfg = Config::from_env();
        let stats = WorktreeStats { extras: 4, stale: 4 };
        let ctx = ctx_with_pr(&input, &cfg, "main", stats);
        let seg = render(&ctx).expect("renders");
        assert_eq!(seg.red_count, 0);
    }

    #[test]
    fn worktree_marker_only_in_full_variant() {
        let input = {
            StatusInput {
                worktree: Some(crate::input::Worktree {
                    name: Some("feat".into()),
                    branch: None,
                    original_branch: Some("main".into()),
                }),
                ..StatusInput::default()
            }
        };
        let cfg = Config::from_env();
        let mut ctx = ctx_with_pr(&input, &cfg, "wt-feat", WorktreeStats::default());
        ctx.in_worktree = true;
        let seg = render(&ctx).expect("renders");
        let full = strip_ansi(&seg.full);
        let compact = strip_ansi(seg.compact.as_deref().expect("has compact"));
        let micro = strip_ansi(seg.micro.as_deref().expect("has micro"));
        assert!(full.contains("[wt ←main]"), "full carries worktree marker");
        assert!(!compact.contains("[wt"), "compact omits the worktree marker");
        assert!(!micro.contains("[wt"), "micro omits the worktree marker");
    }

    #[test]
    fn no_repo_renders_dim_cwd_hint() {
        let input = StatusInput::default();
        let cfg = Config::from_env();
        let mut ctx = RenderContext::test_default(&input, &cfg);
        ctx.cwd = std::path::PathBuf::from("/var/log");
        let seg = render(&ctx).expect("renders even outside repo");
        assert_eq!(strip_ansi(&seg.full), "log");
        assert_eq!(seg.red_count, 0);
    }
}
