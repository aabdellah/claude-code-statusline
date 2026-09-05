# Claude Code Statusline

A statusline tuned for power-user / multi-agent / 1M-context / parallel-worktree
workflows. Renders model · repo · context · effort · rate limits · cache ·
cost · perf · duration into a single scannable line, with a compact fallback
for narrow terminals and a CRIT banner when multiple red signals fire at once.

```
Opus 4.7 · banknet2-retail/main ●3 ↑2 wt:5 2stale #247 · todo +4 ·
ctx 78% ████████░░ 1m · xhigh · 5h 64%→1h12m 7d 71%→98% ·
cache 84% ttl 2:47 · $4.21 $12.4/h +247/-89 $0.017/LOC lpm 49 · 142t/s · api 41% · 47m
```

Written in Rust, ships as a single ~1.6 MB binary with zero runtime
dependencies beyond what every Mac/Linux/Windows machine has by default.
**~6 ms per render** on Apple Silicon (~18× faster than the original Node
version, ~6.7× faster than the first Rust cut that still used `git`
subprocesses). Local git operations use libgit2 directly; only the anthropic
status check involves any non-libgit2 file I/O.

## Install (any machine in under a minute)

Prereq: Rust must be available. Platform-specific installers below; same
behavior on all three.

### macOS / Linux

```bash
git clone git@github.com:aabdellah/claude-code-statusline.git
cd claude-code-statusline
./install.sh                  # or: ./install.sh --with-autobuild
```

Install Rust first if needed:
```bash
brew install rust                                    # macOS
sudo apt install rustc cargo build-essential         # Debian/Ubuntu
sudo dnf install rust cargo gcc                      # Fedora
sudo pacman -S rust base-devel                       # Arch
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # any *nix
```

### Windows (PowerShell)

```powershell
git clone git@github.com:aabdellah/claude-code-statusline.git
cd claude-code-statusline
.\install.ps1                 # or: .\install.ps1 -WithAutobuild
```

Install Rust first if needed:
```powershell
winget install Rustlang.Rustup
```

Windows notes: Claude Code runs the status line through Git Bash when it's
installed (PowerShell otherwise), so the installer writes the binary path
with forward slashes. Width detection uses the console buffer, then the
`COLUMNS` variable Claude Code exports; the tmux / `ps` / PTY fallbacks are
Unix-only. Scratch caches live in `%TEMP%` instead of `/tmp`, and the
account-scoped usage cache in `%LOCALAPPDATA%\cc-statusline`.

### What the installers do (identical across platforms)

1. Build the release binary (~30s on first build — libgit2 vendors itself)
2. Place the binary at the canonical hooks path on this OS
3. Patch your `~/.claude/settings.json` (`%USERPROFILE%\.claude\settings.json`
   on Windows) with the right `statusLine` block — preserves every other
   key untouched

Start a new Claude Code turn and the statusline will appear.

### Auto-rebuild on source edits (optional)

Each platform uses its native job-scheduling system, all running the same
underlying `cargo watch` watcher:

| Platform | `--with-autobuild` uses |
|---|---|
| macOS   | LaunchAgent (`~/Library/LaunchAgents/*.plist`) |
| Linux   | systemd user unit (`~/.config/systemd/user/*.service`) |
| Windows | Scheduled Task (visible in `taskschd.msc`) |

### Updating

Same idempotent path — re-run the installer after pulling new code:

```bash
git pull && ./install.sh           # macOS / Linux
git pull;  .\install.ps1           # Windows
```

### Uninstall

```bash
./install.sh --uninstall           # macOS / Linux
.\install.ps1 -Uninstall           # Windows
```

Removes the symlink/copy, the statusLine block from settings.json (other
keys preserved), and the auto-rebuild job if installed.

## Dependencies on the target machine

- `git` (required for repo state — the binary shells out)
- `curl` (optional — used for `status.claude.com` background fetch; Windows
  10 1803+ ships it in `System32`)
- macOS / Linux: `python3` (used by install.sh to patch settings.json safely;
  bundled with macOS since 12.3, install via your distro's package manager
  on Linux)
- Windows: PowerShell 5.1+ (bundled with Windows 10/11)
- Linux: a C compiler for the vendored libgit2 build (`gcc` /
  `build-essential` / `base-devel` per distro)
- *That's it.* No Node, no Python, no shared libraries beyond `libSystem`
  (macOS) / `glibc` (Linux) / `kernel32` (Windows).

## Configuration (env vars)

| Env var | Effect |
|---|---|
| `STATUSLINE_DEBUG_TIMING=1` | Print per-segment ms to stderr |
| `STATUSLINE_SHOW_PLUGINS=1` | Show `learning+explanatory` plugin styles |
| `STATUSLINE_NO_BLINK=1` | Disable boss-fight blink at ≥90% context |
| `STATUSLINE_HIDE=mileage,perf,duration` | Suppress specific segments |
| `STATUSLINE_MODE=auto\|full\|compact` | `auto` (default) = adaptive layout; `full` = every segment at full text; `compact` = every segment at smallest variant |
| `STATUSLINE_WIDTH=N` | Force terminal width (for testing) |
| `STATUSLINE_WIDTH_MARGIN=N` | Cells subtracted from detected width before fitting (default `4` — Claude Code draws 2 cells of frame on each side of the pane). Set to `0` if using a host without margins. |
| `STATUSLINE_DEBUG_WIDTH=1` | Persist width-detection trace to `/tmp` (`%TEMP%` on Windows) |

## Rate-limit probe for schedulers

The binary doubles as a quota probe for anything that cannot see the status
line — a long-running Claude Code Workflow, a cron job, a build queue. Same
token handling as the render path (keychain / credentials file, fixed-path
`curl`, bearer via curl config, never argv); the caller gets numbers, not
credentials.

```bash
# One JSON document: every window /api/oauth/usage reports, plus pace.
~/.claude/bin/cc-statusline --usage-json
# {"account":"…","fetched_at":"2026-09-04T23:07:10Z","cached":false,
#  "windows":{"five_hour":{"used_percentage":52.0,"resets_at":"…"},
#             "seven_day":{…},"fable":{…},"opus":null,"sonnet":null},
#  "seven_day_pace":{"projected":19.0,"frac_elapsed":0.58},
#  "five_hour_pace":{…},"earliest_reset":"2026-09-05T01:09:59Z","unmet":[]}

# Block until every condition holds, then print the same document.
~/.claude/bin/cc-statusline --wait-until 'five_hour<85,seven_day<92,fable<95,pace<105' [--timeout SECS]
```

Windows: `five_hour`/`5h`, `seven_day`/`7d`, `fable`, `opus`, `sonnet`,
and `pace` (projected end-of-week use of the 7d cap at the current rate).
Operators: `<` and `<=`. A window the account does not have satisfies any
condition on it. Exit codes: 0 satisfied, 2 no data for 30 min, 3 timeout,
64 bad spec. Progress goes to stderr.

Wait mode polls every 5 min, or sooner when an unmet window's reset is
closer, and sleeps in 10 s slices that re-read the active account: a
`/login` to another account wakes it at once and the next fetch shows the
new quota. Cache: `usage-full.json` in the private state dir, 120 s TTL,
account-stamped. Opt out with `STATUSLINE_USAGE_SOURCE=off` (exit 2).

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
│   ├── git.rs             # git status, todo Δ, worktree stats (libgit2)
│   ├── transcript.rs      # JSONL tail reader + derived metrics
│   ├── anthropic.rs       # status.claude.com check (cached + bg refresh)
│   ├── pace.rs            # 7-day rate-limit pace projection
│   ├── probe.rs           # --usage-json / --wait-until quota probe for schedulers
│   ├── platform.rs        # OS seam: home/tmp dirs, detached spawn, local midnight, console width
│   ├── width.rs           # terminal-width detection (8 layered fallbacks)
│   ├── layout.rs          # priority-tiered adaptive fit (full/compact/micro/drop)
│   └── render.rs          # segment assembly
└── docs/
    └── ROADMAP.md         # shipped segments + backlog + rejected ideas
```

## Layout-aware rendering

Every segment declares a **priority** and 1-3 **variants** (full → compact → micro).
At render time, the fitter starts from FULL and downgrades the lowest-priority
segments until the line fits the detected terminal width.

| Tier | Segments | Behavior |
|---|---|---|
| Critical | `model`, `context`, CRIT banner | Never drops; can only downgrade variants |
| Important | `repo/branch`, `cost`, `capabilities`, `anthropic-status` | Drops last |
| Normal | `rate-limits`, `cache`, `duration` | Drops in tight layouts |
| Optional | `yak`, `todo`, `pr`, `wt`, `perf`, `output-style`, `cwd-drift`, `destruction` | Drops first |

Example degradation for a session in a git repo with cost data:

```
width=220 → Opus 4.7 · claude-code-statusline/main ○3+1 · ctx 78% ████████░░ 1m · medium · 5h 64% 7d 71% · cache 84% · $4.21 $5.37/h +247/-89 $0.017/LOC · 47m
width=130 → Opus 4.7 · claude-code-statusline/main ○3+1 · ctx 78% ████████░░ 1m · medium · cache 84% · $4.21 $5.37/h +247/-89 $0.017/LOC · 47m
width=100 → Opus 4.7 · claude-code-statusline/main ○3+1 · ctx 78% ████████░░ 1m · medium · $4.2 $5.4/h +247/-89
width=80  → Opus 4.7 · claude/main ○3+1 · ctx 78% ████████░░ 1m · medium · $4.21
width=60  → Opus 4.7 · main ○3+1 · 78%/1m · medium · $4.21
width=40  → Opus 4.7 · 78%/1m · $4.21
```

## Conventions

- Every segment is gated on data presence — formatters return `Option<String>`
  and a `None` simply omits that segment.
- Git is invoked via `std::process::Command` (no shell, no injection surface).
- 24-bit truecolor used for the context bar; falls back to readable text on
  terminals that don't support it.
- `Config` reads every env var once at startup; nothing else touches env.
- Four runtime deps total: `serde` + `serde_json` (JSON), `regex` (TODO/dest
  patterns), `libc` (Unix only — ioctl(TIOCGWINSZ) for ancestor-PTY width,
  setsid, localtime_r; Windows declares its few kernel32 calls by hand in
  `platform.rs`), `git2` (libgit2 bindings, statically linked via
  vendored-libgit2 feature so the resulting binary doesn't depend on a
  system libgit2).
- Everything OS-specific goes through `src/platform.rs`. New code must not
  reach for `/tmp`, `$HOME`, `std::os::unix`, or `libc` directly.

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
