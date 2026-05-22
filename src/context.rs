//! `RenderContext` — everything a segment renderer needs, computed once
//! upfront so the per-segment functions stay pure-input → optional-Seg.
//!
//! Builds happen lazily-but-once: if we're not in a git repo, `git_status`
//! and `worktree_stats` get their `Default` values (all zeros) and we never
//! call libgit2. Same with `transcript` — if there's no path, an empty Vec.

use std::path::{Path, PathBuf};

use crate::aggregate::{self, TodayRollup};
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

    /// Cross-session today rollup (cost + tokens since local midnight). `None`
    /// when the cache hasn't been populated yet — first render of a session or
    /// just after a cache invalidation. Populated lazily by a detached refresh.
    pub today: Option<TodayRollup>,

    // ─── Pre-computed transcript-derived metrics ────────────────────────────
    // Segments read these instead of re-walking `transcript` per render.
    // Default values (0 / None) mean "no signal" — segments treat them the
    // same as "we didn't bother computing" because both render as empty.
    /// Yak-shave depth at the latest transcript entry. 0 = main thread.
    pub yak_depth: u32,
    /// Count of destructive Bash invocations in the visible transcript tail.
    pub destruction_count: u32,
    /// Approximated cache TTL (ms) remaining in Anthropic's 5-minute prompt
    /// cache window. `None` when no timestamped entries exist.
    pub cache_ttl_ms: Option<i64>,
    /// Output tok/s rate from the most recent assistant turn.
    pub tok_rate: Option<f64>,
    /// Approximate first-token latency (ms) for the most recent assistant turn.
    pub ftl_ms: Option<f64>,

    // ─── Pre-computed I/O outside transcript ────────────────────────────────
    /// Net +/- TODO/FIXME tokens in the working-tree diff. Zero outside a
    /// repo, on a clean tree, or when libgit2 fails — all treated as "no
    /// signal".
    pub todo_delta: i32,
    /// Plugin-injected output styles (learning/explanatory). Empty unless
    /// `STATUSLINE_SHOW_PLUGINS=1`.
    pub plugin_styles: Vec<String>,
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

        // Transcript-derived metrics — pre-compute once so segments stay
        // pure. All operate on the already-loaded `transcript` Vec; the
        // O(N) walks here replace per-segment re-walks.
        let yak_depth = config::timed("yak-depth", cfg.debug_timing, || {
            transcript::yak_depth(&transcript)
        });
        let destruction_count = if in_repo {
            config::timed("destruction", cfg.debug_timing, || {
                transcript::destruction_count(&transcript)
            })
        } else {
            0
        };
        let cache_ttl_ms = transcript::cache_ttl_ms_remaining(&transcript);
        let tok_rate = transcript::last_turn_output_rate(&transcript);
        let ftl_ms = config::timed("ftl", cfg.debug_timing, || {
            transcript::first_token_latency_ms(&transcript)
        });

        // TODO delta — libgit2 diff walk; only relevant when the tree is
        // actually dirty, so guard tight and skip otherwise.
        let todo_delta = if in_repo && git_status.dirty {
            config::timed("todo-delta", cfg.debug_timing, || git::todo_delta(&cwd))
        } else {
            0
        };

        // Plugin-injected output styles — reads ~/.claude/settings.json.
        // Opt-in via STATUSLINE_SHOW_PLUGINS=1.
        let plugin_styles = if cfg.show_plugins {
            config::timed("plugin-styles", cfg.debug_timing, read_plugin_styles)
        } else {
            Vec::new()
        };

        // Today's cross-session $ + tokens. Reads a /tmp cache file; spawns
        // a detached `self --refresh-today` if the cache is stale or missing.
        // Returns `None` on first run until the background refresh lands.
        let today = config::timed("today-rollup", cfg.debug_timing, aggregate::read_today);

        // `gitdir` is computed only to derive `branch` — segments don't
        // need it directly today. If a future segment wants it, add it back
        // as a field and propagate through the build.
        let _ = gitdir;

        Self {
            input, cfg,
            cwd, project_dir, repo_name,
            in_repo, in_worktree, branch,
            git_status, worktree_stats,
            today,
            yak_depth, destruction_count, cache_ttl_ms, tok_rate, ftl_ms,
            todo_delta, plugin_styles,
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
    /// present, else the cwd's last component. Returns `Cow::Borrowed` when
    /// borrowing is possible — only owned when neither source produces a
    /// usable `&str` (`PathBuf::file_name()` returning non-UTF-8).
    pub fn display_repo(&self) -> std::borrow::Cow<'_, str> {
        if let Some(name) = self.repo_name.as_deref() {
            std::borrow::Cow::Borrowed(name)
        } else {
            std::borrow::Cow::Borrowed(self.cwd_name())
        }
    }

    /// Borrow cwd as a Path for libgit2 calls.
    pub fn cwd_path(&self) -> &Path {
        &self.cwd
    }

    /// Test-only constructor: produces a no-I/O context with all derived
    /// fields zeroed/None. Mutate the returned struct to set up specific
    /// scenarios. Keeps segment unit tests boilerplate-free.
    #[cfg(test)]
    pub(crate) fn test_default<'b>(input: &'b StatusInput, cfg: &'b Config) -> RenderContext<'b> {
        RenderContext {
            input,
            cfg,
            cwd: std::path::PathBuf::from("/tmp"),
            project_dir: None,
            repo_name: None,
            in_repo: false,
            in_worktree: false,
            branch: None,
            git_status: GitStatus::default(),
            worktree_stats: WorktreeStats::default(),
            today: None,
            yak_depth: 0,
            destruction_count: 0,
            cache_ttl_ms: None,
            tok_rate: None,
            ftl_ms: None,
            todo_delta: 0,
            plugin_styles: Vec::new(),
        }
    }
}

/// Read `~/.claude/settings.json` and extract any enabled plugin-injected
/// output styles (currently "learning" and "explanatory"). I/O lives in
/// `RenderContext::build` so segments stay pure.
fn read_plugin_styles() -> Vec<String> {
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
            styles.push("learning".to_owned());
        } else if key.starts_with("explanatory-output-style@") {
            styles.push("explanatory".to_owned());
        }
    }
    styles
}
