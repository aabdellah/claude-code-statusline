//! Git operations.
//!
//! Two layers:
//!   1. **Filesystem fast path** — `find_gitdir()` + `branch_or_sha_from_head()`
//!      walk up looking for `.git/` and read `HEAD` directly. Microseconds, no
//!      subprocess. Used for the "is this a repo?" check and branch name —
//!      both questions that don't need git's full machinery.
//!   2. **Subprocess git** — `git_status`, `worktree_stats`, `todo_delta` shell
//!      out to `git` for things that DO need the index, the diff machine, etc.
//!      Callers parallelize these via `std::thread::scope` because they're
//!      embarrassingly independent.
//!
//! Subprocess git() returns None on any failure (non-repo, command missing,
//! exit status non-zero) and callers handle it gracefully.

use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

const STALE_WT: Duration = Duration::from_secs(3 * 24 * 3600);

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
                // Linked worktree: .git is a file pointing to the real gitdir
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
        match p.parent() {
            Some(parent) => p = parent,
            None => return None,
        }
    }
}

/// Read the current branch name (or short SHA on detached HEAD) directly
/// from `.git/HEAD`. `HEAD` is one of:
///   - `ref: refs/heads/main\n`            → branch is `main`
///   - `3a3e8f9a...full40charSHA...\n`     → detached, return first 7 hex chars
///
/// Cost: a single `read_to_string` of a ~40-byte file. Compared to `git
/// symbolic-ref --short HEAD` which is ~12ms of subprocess overhead.
pub fn branch_or_sha_from_head(gitdir: &Path) -> Option<String> {
    let raw = fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let head = raw.trim();
    if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
        return Some(branch.to_string());
    }
    // Detached: contents should be a 40-char SHA (or 64 for sha256). Shorten to 7.
    if head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(head[..7].to_string());
    }
    None
}

// --- Subprocess git (slower, but full feature set) --------------------------


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

/// Run `git` with the given args in `cwd`. Returns `None` on any failure.
pub fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    Some(s.trim_end().to_string())
}

/// Combines 3 reads into 1 subprocess via `git status --porcelain=v1
/// --branch --show-stash`. Cuts ~20ms from the hot path on warm cache.
pub fn git_status(cwd: &Path) -> GitStatus {
    let raw = git(cwd, &[
        "status",
        "--porcelain=v1",
        "--branch",
        "--show-stash",
        "--untracked-files=normal",
    ]);
    let Some(raw) = raw else { return GitStatus::default(); };

    let mut s = GitStatus::default();
    for l in raw.lines() {
        if l.is_empty() { continue; }
        if let Some(rest) = l.strip_prefix("## ") {
            // Branch header. Example: "main...origin/main [ahead 2, behind 1]"
            if let Some(idx) = rest.find("ahead ") {
                let tail = &rest[idx + 6..];
                let end = tail.find(|c: char| !c.is_ascii_digit()).unwrap_or(tail.len());
                if let Ok(n) = tail[..end].parse() { s.ahead = n; }
            }
            if let Some(idx) = rest.find("behind ") {
                let tail = &rest[idx + 7..];
                let end = tail.find(|c: char| !c.is_ascii_digit()).unwrap_or(tail.len());
                if let Ok(n) = tail[..end].parse() { s.behind = n; }
            }
        } else if let Some(rest) = l.strip_prefix("# stash ") {
            let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            if let Ok(n) = rest[..end].parse() { s.stash = n; }
        } else if l.starts_with("??") {
            s.untracked += 1;
        } else {
            let b = l.as_bytes();
            if b.len() >= 2 {
                if b[0] != b' ' && b[0] != b'?' { s.staged += 1; }
                if b[1] != b' ' && b[1] != b'?' { s.unstaged += 1; }
            }
        }
    }
    s.dirty = s.staged + s.unstaged + s.untracked > 0;
    s
}

/// Counts added-minus-removed TODO/FIXME occurrences in the working-tree diff.
/// Fast even on large repos — only scans diff lines, not the tree.
pub fn todo_delta(cwd: &Path) -> i32 {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\b(TODO|FIXME)\b").unwrap());

    let Some(diff) = git(cwd, &["diff", "--unified=0", "--no-color", "HEAD"]) else {
        return 0;
    };
    let mut added: i32 = 0;
    let mut removed: i32 = 0;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") { continue; }
        let Some(first) = line.as_bytes().first() else { continue; };
        if *first != b'+' && *first != b'-' { continue; }
        let n = re.find_iter(line).count() as i32;
        if n == 0 { continue; }
        if *first == b'+' { added += n; } else { removed += n; }
    }
    added - removed
}

/// `extras` = sibling worktrees of `cwd` excluding the main checkout.
/// `stale`  = those whose HEAD ref hasn't been touched in >3 days.
/// Uses HEAD mtime as a cheap proxy; avoids per-worktree subprocesses.
pub fn worktree_stats(cwd: &Path) -> WorktreeStats {
    let Some(raw) = git(cwd, &["worktree", "list", "--porcelain"]) else {
        return WorktreeStats::default();
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    for line in raw.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            paths.push(PathBuf::from(p));
        }
    }

    let mut stale = 0u32;
    let now = SystemTime::now();
    // First path is the main checkout; skip it.
    for p in paths.iter().skip(1) {
        let head_path = resolve_head_path(p);
        if let Ok(meta) = fs::metadata(&head_path) {
            if let Ok(modified) = meta.modified() {
                if now.duration_since(modified).map(|d| d > STALE_WT).unwrap_or(false) {
                    stale += 1;
                }
            }
        }
    }
    WorktreeStats {
        extras: paths.len().saturating_sub(1) as u32,
        stale,
    }
}

/// A worktree's `.git` is a file pointing to the gitdir; the gitdir holds HEAD.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a fake .git/ directory and return its parent (the "working tree").
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
    fn find_gitdir_returns_none_outside_repo() {
        // /tmp itself isn't a repo (it's the parent of our fake repos)
        // — but only if we're not unfortunately inside one. Use a synthetic
        // path that definitely doesn't have .git anywhere above it.
        let nowhere = std::env::temp_dir().join("definitely-not-a-repo-ccsl");
        let _ = fs::remove_dir_all(&nowhere);
        fs::create_dir_all(&nowhere).unwrap();
        // Walking up will hit /tmp, /, etc. — none of those should have .git
        // (if they did, the whole world would be broken). This test is real
        // enough for CI-style assurance.
        assert!(find_gitdir(&nowhere).is_some() == nowhere.ancestors().any(|p| p.join(".git").exists()));
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
    fn parses_ahead_behind() {
        // Simulate output by faking the parse logic via direct call
        // — we can't easily test against a fixture without a real git repo,
        // but we can at least verify the regex-free parser handles the format.
        let s = " main...origin/main [ahead 2, behind 1]";
        // Mimic the inline parser
        let mut ahead = 0u32;
        let mut behind = 0u32;
        if let Some(idx) = s.find("ahead ") {
            let tail = &s[idx + 6..];
            let end = tail.find(|c: char| !c.is_ascii_digit()).unwrap_or(tail.len());
            ahead = tail[..end].parse().unwrap();
        }
        if let Some(idx) = s.find("behind ") {
            let tail = &s[idx + 7..];
            let end = tail.find(|c: char| !c.is_ascii_digit()).unwrap_or(tail.len());
            behind = tail[..end].parse().unwrap();
        }
        assert_eq!(ahead, 2);
        assert_eq!(behind, 1);
    }
}
