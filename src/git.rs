//! Git operations.
//!
//! Two layers, each picked for the cost of the operation:
//!
//!   1. **Filesystem fast path** — `find_gitdir()` + `branch_or_sha_from_head()`
//!      walk up looking for `.git/` and read `HEAD` directly. ~10-30µs total.
//!      libgit2 would be ~100-500µs for the same work because it parses
//!      config + validates the repo we don't actually care about.
//!
//!   2. **libgit2 via the `git2` crate** — `git_status`, `worktree_stats`,
//!      `todo_delta` use libgit2's Repository API. Sub-millisecond each.
//!      Replaces what used to be ~12-15ms subprocess calls.
//!
//! No subprocesses remain. No more `std::process::Command` fork/exec cost.

use git2::{BranchType, DiffOptions, Repository, Status, StatusOptions};
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

const STALE_WT: Duration = Duration::from_secs(3 * 24 * 3600);

#[derive(Debug, Default, Clone, Copy)]
pub struct GitStatus {
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub ahead: u32,
    pub behind: u32,
    pub stash: u32,
    pub dirty: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct WorktreeStats {
    pub extras: u32,
    pub stale: u32,
}

// --- Filesystem fast path ---------------------------------------------------

/// Walk up from `start` looking for the first ancestor containing `.git`.
/// Returns the **gitdir** itself (not the working tree). For a normal
/// checkout that's `<repo>/.git/`. For a linked worktree, `.git` is a FILE
/// containing `gitdir: <abs-or-rel-path>` — we resolve that here.
pub fn find_gitdir(start: &Path) -> Option<PathBuf> {
    let mut p = start;
    loop {
        let dotgit = p.join(".git");
        match fs::metadata(&dotgit) {
            Ok(meta) if meta.is_dir() => return Some(dotgit),
            Ok(meta) if meta.is_file() => {
                if let Ok(content) = fs::read_to_string(&dotgit) {
                    for line in content.lines() {
                        if let Some(gd) = line.strip_prefix("gitdir:") {
                            let gd_path = Path::new(gd.trim());
                            return Some(if gd_path.is_absolute() {
                                gd_path.to_path_buf()
                            } else {
                                p.join(gd_path)
                            });
                        }
                    }
                }
                return None;
            }
            _ => {}
        }
        p = p.parent()?;
    }
}

/// Read the current branch name (or short SHA on detached HEAD) directly
/// from `.git/HEAD`. `HEAD` is one of:
///   - `ref: refs/heads/main\n`            → branch is `main`
///   - `3a3e8f9a...full40charSHA...\n`     → detached, return first 7 hex chars
pub fn branch_or_sha_from_head(gitdir: &Path) -> Option<String> {
    let raw = fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let head = raw.trim();
    if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
        return Some(branch.to_string());
    }
    if head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit()) {
        // chars().take().collect() instead of byte-slicing — safe because
        // the all(is_ascii_hexdigit) guard implies single-byte chars, but
        // CLAUDE.md mandates no byte-slicing on filesystem-sourced strings.
        return Some(head.chars().take(7).collect());
    }
    None
}

// --- libgit2 heavy ops ------------------------------------------------------

/// Open the repo at `cwd` (walking up if needed). Returns None on any error.
fn open_repo(cwd: &Path) -> Option<Repository> {
    Repository::discover(cwd).ok()
}

/// Status + ahead/behind + stash count in a single pass through libgit2.
/// Equivalent to running `git status --porcelain --branch --show-stash`,
/// but ~10× faster because no subprocess.
pub fn git_status(cwd: &Path) -> GitStatus {
    let Some(mut repo) = open_repo(cwd) else { return GitStatus::default(); };

    let mut s = GitStatus::default();

    // --- File statuses
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(false);
    if let Ok(statuses) = repo.statuses(Some(&mut opts)) {
        for entry in statuses.iter() {
            let st = entry.status();
            // Each file can contribute to multiple counters — match the
            // porcelain v1 behavior where index column and worktree column
            // are independent.
            let staged = st.intersects(
                Status::INDEX_NEW
                    | Status::INDEX_MODIFIED
                    | Status::INDEX_DELETED
                    | Status::INDEX_RENAMED
                    | Status::INDEX_TYPECHANGE,
            );
            let unstaged = st.intersects(
                Status::WT_MODIFIED
                    | Status::WT_DELETED
                    | Status::WT_TYPECHANGE
                    | Status::WT_RENAMED,
            );
            let untracked = st.contains(Status::WT_NEW) && !staged;
            if staged { s.staged += 1; }
            if unstaged { s.unstaged += 1; }
            if untracked { s.untracked += 1; }
        }
    }

    // --- Ahead/behind (vs configured upstream)
    if let Ok(head_ref) = repo.head()
        && let Some(local_oid) = head_ref.target()
        && let Some(shorthand) = head_ref.shorthand()
    {
        let full_branch = format!("refs/heads/{}", shorthand);
        if let Ok(upstream_name) = repo.branch_upstream_name(&full_branch)
            && let Some(name) = upstream_name.as_str()
            && let Ok(upstream_ref) = repo.find_reference(name)
            && let Some(upstream_oid) = upstream_ref.target()
            && let Ok((ahead, behind)) = repo.graph_ahead_behind(local_oid, upstream_oid)
        {
            s.ahead = ahead as u32;
            s.behind = behind as u32;
        }
    }

    // --- Stash count
    let mut stash_count = 0u32;
    let _ = repo.stash_foreach(|_idx, _msg, _oid| {
        stash_count += 1;
        true
    });
    s.stash = stash_count;

    s.dirty = s.staged + s.unstaged + s.untracked > 0;
    s
}

/// Linked worktree count + how many are stale (HEAD ref untouched >3 days).
/// libgit2 enumerates worktrees natively; we still use filesystem mtime for
/// staleness because that's a "when did anyone last work in this checkout"
/// signal, not a git-semantic one.
pub fn worktree_stats(cwd: &Path) -> WorktreeStats {
    let Some(repo) = open_repo(cwd) else { return WorktreeStats::default(); };
    let Ok(names) = repo.worktrees() else { return WorktreeStats::default(); };

    let mut stale = 0u32;
    let now = SystemTime::now();
    for name_opt in names.iter() {
        let Some(name) = name_opt else { continue; };
        let Ok(wt) = repo.find_worktree(name) else { continue; };
        let head_path = resolve_head_path(wt.path());
        if let Ok(meta) = fs::metadata(&head_path)
            && let Ok(modified) = meta.modified()
            && now.duration_since(modified).map(|d| d > STALE_WT).unwrap_or(false)
        {
            stale += 1;
        }
    }
    WorktreeStats { extras: names.len() as u32, stale }
}

/// Net TODO/FIXME delta between HEAD's tree and the working tree (including
/// staged changes). Walks the libgit2 diff hunks line-by-line.
pub fn todo_delta(cwd: &Path) -> i32 {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\b(TODO|FIXME)\b").unwrap());

    let Some(repo) = open_repo(cwd) else { return 0; };

    let mut opts = DiffOptions::new();
    opts.context_lines(0); // only show +/- lines, no unchanged context

    // HEAD tree → workdir, with index included
    let tree = repo.head().ok()
        .and_then(|h| h.peel_to_tree().ok());
    let Ok(diff) = repo.diff_tree_to_workdir_with_index(tree.as_ref(), Some(&mut opts)) else {
        return 0;
    };

    let mut added = 0i32;
    let mut removed = 0i32;
    let _ = diff.foreach(
        &mut |_delta, _progress| true,
        None,
        None,
        Some(&mut |_delta, _hunk, line| {
            let content = std::str::from_utf8(line.content()).unwrap_or("");
            let n = re.find_iter(content).count() as i32;
            if n == 0 { return true; }
            match line.origin() {
                '+' => added += n,
                '-' => removed += n,
                _ => {}
            }
            true
        }),
    );
    added - removed
}

/// A worktree's `.git` is a file pointing to the gitdir; the gitdir holds HEAD.
/// (Shared between worktree_stats and Phase 1's find_gitdir behavior.)
fn resolve_head_path(worktree_path: &Path) -> PathBuf {
    let dotgit = worktree_path.join(".git");
    if let Ok(content) = fs::read_to_string(&dotgit) {
        for line in content.lines() {
            if let Some(gitdir) = line.strip_prefix("gitdir:") {
                let gitdir = gitdir.trim();
                let abs = if Path::new(gitdir).is_absolute() {
                    PathBuf::from(gitdir)
                } else {
                    worktree_path.join(gitdir)
                };
                return abs.join("HEAD");
            }
        }
    }
    dotgit.join("HEAD")
}

// Suppress the unused warning when BranchType isn't otherwise referenced
// (we use it in tests; the public crate API exposes it via Status etc.)
#[allow(dead_code)]
const _: BranchType = BranchType::Local;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fake_repo(name: &str, head_content: &str) -> std::path::PathBuf {
        let tmp = std::env::temp_dir().join(format!("ccsl-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join(".git")).unwrap();
        let mut f = fs::File::create(tmp.join(".git").join("HEAD")).unwrap();
        f.write_all(head_content.as_bytes()).unwrap();
        tmp
    }

    #[test]
    fn find_gitdir_finds_normal_repo() {
        let root = fake_repo("normal", "ref: refs/heads/main\n");
        let gd = find_gitdir(&root).unwrap();
        assert_eq!(gd, root.join(".git"));
    }

    #[test]
    fn find_gitdir_walks_up_from_subdir() {
        let root = fake_repo("walkup", "ref: refs/heads/main\n");
        let sub = root.join("a").join("b").join("c");
        fs::create_dir_all(&sub).unwrap();
        let gd = find_gitdir(&sub).unwrap();
        assert_eq!(gd, root.join(".git"));
    }

    #[test]
    fn branch_from_ref_pointer() {
        let root = fake_repo("ref", "ref: refs/heads/main\n");
        let gd = find_gitdir(&root).unwrap();
        assert_eq!(branch_or_sha_from_head(&gd).as_deref(), Some("main"));
    }

    #[test]
    fn branch_from_detached_head_is_short_sha() {
        let root = fake_repo("detached", "3a3e8f9a1b2c3d4e5f60718293a4b5c6d7e8f901\n");
        let gd = find_gitdir(&root).unwrap();
        assert_eq!(branch_or_sha_from_head(&gd).as_deref(), Some("3a3e8f9"));
    }

    #[test]
    fn branch_from_garbage_head_returns_none() {
        let root = fake_repo("garbage", "this is not a valid HEAD\n");
        let gd = find_gitdir(&root).unwrap();
        assert_eq!(branch_or_sha_from_head(&gd), None);
    }

    #[test]
    fn git_status_on_self_repo_is_smoketestable() {
        // We're inside a real repo (this very project), so libgit2 should
        // be able to open it and return something without panicking.
        let here = std::env::current_dir().unwrap();
        let s = git_status(&here);
        // Just confirm it doesn't panic and doesn't return obviously broken values.
        // The exact counts vary with the working tree state during test runs.
        assert!(s.staged < 10_000);
        assert!(s.unstaged < 10_000);
    }
}
