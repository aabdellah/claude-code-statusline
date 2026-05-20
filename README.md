# Claude Code Statusline

A statusline tuned for power-user / multi-agent / 1M-context / parallel-worktree
workflows. Renders model · repo · context · effort · rate limits · cache ·
cost · perf · duration into a single scannable line, with a compact fallback
for narrow terminals and a CRIT banner when multiple red signals fire at once.

```
Opus 4.7 · banknet2-retail/main ●3 ↑2 wt:5 2stale #247 · todo +4 ·
ctx 78% ████████░░ 1m · xhigh thinking · 5h 64%→1h12m 7d 71%→98% ·
cache 84% ttl 2:47 · $4.21 $12.4/h +247/-89 $0.017/LOC mpt 14 · 142t/s · 47m
```

Written in Rust, ships as a single ~1.6 MB binary with zero runtime
dependencies beyond what every Mac/Linux machine has by default. **~6 ms per
render** on Apple Silicon (~18× faster than the original Node version, ~6.7×
faster than the first Rust cut that still used `git` subprocesses). Local
git operations use libgit2 directly; only the anthropic status check
involves any non-libgit2 file I/O.

## Install

### Prerequisite — Rust toolchain

```bash
brew install rust          # macOS
# or:
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # any *nix
```

### Build + install the binary

```bash
# From the source directory:
cargo build --release

# Drop the binary somewhere stable:
mkdir -p ~/.claude/bin
cp target/release/statusline ~/.claude/bin/cc-statusline
```

Wire it into `~/.claude/settings.json` (use `$HOME` — CC doesn't expand `~`):

```json
{
  "statusLine": {
    "type": "command",
    "command": "$HOME/.claude/bin/cc-statusline"
  }
}
```

Restart Claude Code (or just start a new turn — settings reload between turns).

## Dependencies on the target machine

- `git` (required for repo state — the binary shells out)
- `curl` (optional — used for `status.claude.com` background fetch)
- *That's it.* No Node, no Python, no shared libraries beyond `libSystem`
  (macOS) / `glibc` (Linux).

## Configuration (env vars)

| Env var | Effect |
|---|---|
| `STATUSLINE_DEBUG_TIMING=1` | Print per-segment ms to stderr |
| `STATUSLINE_SHOW_PLUGINS=1` | Show `learning+explanatory` plugin styles |
| `STATUSLINE_NO_BLINK=1` | Disable boss-fight blink at ≥90% context |
| `STATUSLINE_HIDE=mileage,perf,duration` | Suppress specific segments |
| `STATUSLINE_MODE=auto\|full\|compact` | Force compact/full layout |
| `STATUSLINE_COMPACT_BELOW=140` | Width threshold for auto-compact |
| `STATUSLINE_WIDTH=N` | Force terminal width (for testing) |
| `STATUSLINE_DEBUG_WIDTH=1` | Persist width-detection trace to `/tmp` |

## Source layout

```
claude-code-statusline/
├── Cargo.toml             # crate manifest + release profile (opt-z, LTO, strip)
├── src/
│   ├── main.rs            # Entry — read stdin → parse → render → write stdout
│   ├── ansi.rs            # color codes, gradient, gradient_bar, visible_length
│   ├── config.rs          # env-var Config + per-render timing instrumentation
│   ├── input.rs           # typed serde structs for the CC JSON input
│   ├── format.rs          # null-safe value formatters (full + compact)
│   ├── git.rs             # git status, todo Δ, worktree stats
│   ├── transcript.rs      # JSONL tail reader + derived metrics
│   ├── anthropic.rs       # status.claude.com check (cached + bg refresh)
│   ├── pace.rs            # 7-day rate-limit pace projection
│   ├── width.rs           # terminal-width detection (8 layered fallbacks)
│   └── render.rs          # segment assembly + mode selection
└── docs/
    └── ROADMAP.md         # shipped segments + backlog + rejected ideas
```

## Conventions

- Every segment is gated on data presence — formatters return `Option<String>`
  and a `None` simply omits that segment.
- Git is invoked via `std::process::Command` (no shell, no injection surface).
- 24-bit truecolor used for the context bar; falls back to readable text on
  terminals that don't support it.
- `Config` reads every env var once at startup; nothing else touches env.
- Four runtime deps total: `serde` + `serde_json` (JSON), `regex` (TODO/dest
  patterns), `libc` (ioctl(TIOCGWINSZ) for ancestor-PTY width), `git2`
  (libgit2 bindings, statically linked via vendored-libgit2 feature so the
  resulting binary doesn't depend on a system libgit2).

## Performance

```
$ STATUSLINE_DEBUG_TIMING=1 ./target/release/statusline < input.json
[statusline:timing] total=6.2ms 200cols mode=auto→full len=86
     2.14ms  todo-delta      (libgit2 diff)
     1.91ms  git-status      (libgit2 status + ahead/behind + stash)
     1.42ms  anthropic-status (stat /tmp/cc-anthropic-status.json)
     0.58ms  destruction      (transcript scan for rm/drop/--hard)
     0.12ms  worktree-stats   (libgit2 worktree enumeration)
     0.01ms  gitdir-discover  (walk up looking for .git)
     0.00ms  width-detect, transcript-read, ftl, yak-depth
```

Three optimization layers got us here from the original ~40-105ms:

| Version | In-repo | No-repo | Notes |
|---|---|---|---|
| Node.js (original) | ~105ms | ~80ms | Node startup dominated |
| Rust (subprocess git, sequential) | ~40ms | ~25ms | Fork/exec to `git` 3-4× per render |
| Rust (subprocess git, parallel) | ~20ms | ~1.5ms | `std::thread::scope` for git calls |
| Rust (libgit2 + fs fast-path) | **~6ms** | **~1.5ms** | Current — no subprocess at all |

The fast path: when `.git/` isn't found, all git work is skipped entirely.
When found, libgit2 is called directly (no fork/exec) and the branch name
comes from a 40-byte read of `.git/HEAD` rather than `git symbolic-ref`.

## Tests

```bash
cargo test
```

25 unit tests covering ANSI escapes, RFC3339 parsing, formatters, model-name
extraction, yak depth, destruction counter, pace color thresholds.

## Roadmap

See [`docs/ROADMAP.md`](docs/ROADMAP.md) for what's shipped, what's planned
next, the backlog of ideas, and what we've explicitly decided not to build.
