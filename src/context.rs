//! `RenderContext` — everything a segment renderer needs, computed once
//! upfront so the per-segment functions stay pure-input → optional-Seg.
//!
//! Builds happen lazily-but-once: if we're not in a git repo, `git_status`
//! and `worktree_stats` get their `Default` values (all zeros) and we never
//! call libgit2. Same with `transcript` — if there's no path, an empty Vec.

use std::path::{Path, PathBuf};

use crate::config::{self, Config};
use crate::git::{self, GitStatus, WorktreeStats};
use crate::input::StatusInput;
use crate::transcript;

/// All the precomputed state per render. Segments read this and produce a
/// `Seg`; they never touch I/O directly.
pub struct RenderContext<'a> {
    pub input: &'a StatusInput,
    pub cfg: &'a Config,

    pub cwd: PathBuf,
    pub project_dir: Option<String>,
    pub repo_name: Option<String>,

    pub in_repo: bool,
    pub in_worktree: bool,
    pub branch: Option<String>,

    pub git_status: GitStatus,
    pub worktree_stats: WorktreeStats,

    pub transcript: Vec<serde_json::Value>,
}

impl<'a> RenderContext<'a> {
    /// Build the context. All I/O happens here, once. Subsequent segment
    /// renders are pure transforms.
    pub fn build(input: &'a StatusInput, cfg: &'a Config) -> Self {
        // cwd: prefer the workspace's current_dir, then the input's cwd,
        // then the process's actual cwd as a last resort.
        let cwd: PathBuf = input
            .workspace
            .as_ref()
            .and_then(|w| w.current_dir.clone())
            .or_else(|| input.cwd.clone())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

        let project_dir = input
            .workspace
            .as_ref()
            .and_then(|w| w.project_dir.clone());

        let repo_name = input
            .workspace
            .as_ref()
            .and_then(|w| w.repo.as_ref())
            .and_then(|r| r.name.clone());

        let gitdir = config::timed("gitdir-discover", cfg.debug_timing, || {
            git::find_gitdir(&cwd)
        });

        let branch = input
            .worktree
            .as_ref()
            .and_then(|w| w.branch.clone())
            .or_else(|| gitdir.as_deref().and_then(git::branch_or_sha_from_head));

        let in_repo = repo_name.is_some() || gitdir.is_some();
        let in_worktree = input.worktree.as_ref().map(|w| w.name.is_some()).unwrap_or(false)
            || input
                .workspace
                .as_ref()
                .and_then(|w| w.git_worktree.as_deref())
                .is_some();

        // libgit2 calls are sub-ms each; sequential is fine.
        let (git_status, worktree_stats) = if in_repo {
            let s = config::timed("git-status", cfg.debug_timing, || git::git_status(&cwd));
            let w = config::timed("worktree-stats", cfg.debug_timing, || git::worktree_stats(&cwd));
            (s, w)
        } else {
            (GitStatus::default(), WorktreeStats::default())
        };

        let transcript = config::timed("transcript-read", cfg.debug_timing, || {
            transcript::read_transcript_tail(input.transcript_path.as_deref())
        });

        // `gitdir` is computed only to derive `branch` — segments don't
        // need it directly today. If a future segment wants it, add it back
        // as a field and propagate through the build.
        let _ = gitdir;

        Self {
            input, cfg,
            cwd, project_dir, repo_name,
            in_repo, in_worktree, branch,
            git_status, worktree_stats,
            transcript,
        }
    }

    /// Convenience: cwd's last path component as a &str.
    pub fn cwd_name(&self) -> &str {
        self.cwd
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
    }

    /// Resolve "what repo name to display" using the schema's repo.name if
    /// present, else the cwd's last component.
    pub fn display_repo(&self) -> String {
        self.repo_name.clone().unwrap_or_else(|| self.cwd_name().to_string())
    }

    /// Borrow cwd as a Path for libgit2 calls.
    pub fn cwd_path(&self) -> &Path {
        &self.cwd
    }
}
