# Roadmap

What ships today, what's queued next, what's on the backlog, and what we've
explicitly decided not to build. Original brainstorm curated from research
across `ccstatusline`, `ccusage`, `CCometixLine`, `claude-powerline`,
`rz1989s/claude-code-statusline`, plus original ideas.

> **Migration note (2026-05-20):** Ported from Node.js to Rust. All segments
> render byte-identically (modulo the intentionally-random boss-fight
> flicker). The binary is ~1.2 MB, single file, zero runtime deps beyond
> system libc.

---

## ✅ Shipped

Everything the statusline renders today. Each entry points to the module
where it lives.

### Layout core

| Segment | Where | Notes |
|---|---|---|
| Model name | `src/format.rs` :: `short_model_name` | Strips `(1M context)` suffix; falls back to model.id derivation |
| Repo / branch | `src/git.rs` + `src/render.rs` | Branch color encodes dirty + main/master state |
| Git flags | `src/git.rs` :: `git_status` | Single `git status --porcelain --branch --show-stash` call |
| Ahead / behind | `src/git.rs` :: `git_status` | `↓3+` escalates to red-signal |
| Stash count | `src/git.rs` :: `git_status` | `⚑N` |
| TODO/FIXME Δ | `src/git.rs` :: `todo_delta` | Skipped on clean tree (saves ~7ms) |
| Worktree marker | `src/render.rs` | `[wt ←origin]` when inside a worktree |
| Worktree extras + stale | `src/git.rs` :: `worktree_stats` | `wt:5 2stale`; stale = HEAD mtime >3d |
| PR badge | `src/render.rs` | Color encodes review state |
| `cwd≠proj` drift warning | `src/render.rs` | When CC's cwd has wandered off project_dir |

### Context + capabilities

| Segment | Where | Notes |
|---|---|---|
| Context % + gradient bar | `src/ansi.rs` :: `gradient_bar` | 24-bit truecolor green→yellow→red |
| Boss-fight mode | `src/ansi.rs` :: `gradient_bar` | ≥85% crit red + ▒ damage cells; ≥90% adds ANSI blink + █/▓ flicker |
| Context window size | `src/format.rs` :: `fmt_ctx_size` | `1m` / `1.5m` / `200k` |
| `200k+` overflow warn | `src/render.rs` | Red-signal |
| Effort level | `src/segments/capabilities.rs` | `max` / `xhigh` are red-signal. Subsumes thinking — CC's `/effort` controls both. |
| Fast mode | `src/segments/capabilities.rs` | Bright magenta bold |
| Output style + plugins | `src/render.rs` :: `active_plugin_styles` | Plugin styles opt-in via `STATUSLINE_SHOW_PLUGINS=1` |

### Rate limits + cost

| Segment | Where | Notes |
|---|---|---|
| 5h limit + reset time | `src/format.rs` :: `fmt_reset_time` | `5h 30%→2h15m` |
| 7d limit + pace projection | `src/pace.rs` :: `seven_day_pace` | Asymmetric 92-105% green band; needs ≥10% of window elapsed |
| Fable 5 dedicated weekly (+ Opus/Sonnet scoped) | `src/segments/rate_limits.rs` :: `weekly_window` + `src/usage.rs` | `fable 42% →98%` / `f5:42/98`; same pace math as 7d, but no underpace red (idle scoped windows aren't a problem). Sourced from `rate_limits.seven_day_overage_included` when CC forwards it (not yet as of v2.1.201 — see CLAUDE.md gotcha); until then bridged from `GET /api/oauth/usage` via the detached refresh (cache `/tmp/cc-statusline-usage.json`, 120s TTL; opt out `STATUSLINE_USAGE_SOURCE=off`). |
| Quota probe for schedulers | `src/probe.rs` | `--usage-json` (all `/api/oauth/usage` windows + 5h/7d pace, account-stamped 120 s cache) and `--wait-until '5h<85,7d<92,fable<95,pace<105'` (blocks; polls 5 min or the nearest reset; 10 s slices re-read the account so a `/login` wakes it). Built for Workflow gates on the Studio-bound Shell programme. |
| Cache hit % | `src/render.rs` | From `cw.current_usage` cache_read/create |
| Cache TTL countdown | `src/transcript.rs` :: `cache_ttl_ms_remaining` | 5min window from last timestamped transcript entry |
| Cost (total) | `src/format.rs` :: `fmt_money` | `$1.23` / `$24` / `$1.0k` (compact) |
| Burn rate | `src/format.rs` :: `fmt_burn_rate` | Needs ≥30s session duration for stable rate |
| Lines +/- | `src/render.rs` | From `cost.total_lines_added/removed` |
| $/LOC | `src/format.rs` :: `fmt_dollars_per_loc` | Hidden if <50 LOC (denominator noise) |
| Token mileage | `src/format.rs` :: `fmt_mileage` | `mpt 16` — LOC accepted per 1k tokens |
| Today $ + tokens | `src/segments/today_spend.rs` + `src/aggregate.rs` + `src/pricing.rs` | Cross-session rollup since LOCAL midnight. Renders `today $12.4 1.2m`. Tokens are ground-truth (summed from JSONL `usage.*`); $ uses **live LiteLLM pricing** (fetched 24h-cached from BerriAI), with Claude 4-family embedded constants as fallback when LiteLLM is unreachable. Rollup cache: `/tmp/cc-statusline-today.json`, 60s TTL, refreshed by detached `self --refresh-today`. Pricing cache: `/tmp/cc-statusline-pricing.json`, 24h TTL, refreshed synchronously inside the same detached process (network blocking is fine there). mtime-filters JSONLs older than today's midnight so we never open dormant files. Full scan ~450ms (incl. LiteLLM fetch on day rollover); cache read ~1ms. Opt out of network fetch with `STATUSLINE_PRICING_SOURCE=offline`. |

### Performance + meta

| Segment | Where | Notes |
|---|---|---|
| Output tok/s | `src/transcript.rs` :: `last_turn_output_rate` | Bounded to [0.5s, 1h] turn duration |
| First-token latency | `src/transcript.rs` :: `first_token_latency_ms` | Approximation: turn_time − (tokens/150 t/s); ≥2s threshold |
| Session duration | `src/format.rs` :: `fmt_duration` | `47m12s` / `3h12m` (compact: `47m` / `3h`) |
| Anthropic status | `src/anthropic.rs` | Cached 5min in /tmp; detached background curl + atomic rename via reconcile-on-next-render |

### Behavioral / agent signals

| Segment | Where | Notes |
|---|---|---|
| Yak shave depth | `src/transcript.rs` :: `yak_depth` | Walks sourceToolAssistantUUID chain; `yak~~:3` with growing tildes |
| Destruction counter | `src/transcript.rs` :: `destruction_count` | rm / unlink / truncate / DROP / --force / --hard |
| CRIT banner + red tint | `src/render.rs` | Fires at ≥3 simultaneous red signals; recolors separators |

### Plumbing

| Feature | Where | Notes |
|---|---|---|
| Terminal width detection | `src/width.rs` :: `detect_term_width` | 8-layered fallback: stdout → tmux → ancestor TTY walk → /dev/tty → stty → $COLUMNS → per-PTY cache → shared cache. Uses libc::ioctl(TIOCGWINSZ) for direct PTY queries. |
| Auto compact/full mode | `src/render.rs` | Below `STATUSLINE_COMPACT_BELOW` (default 140) or when full line overflows |
| Per-segment hiding | `src/config.rs` | `STATUSLINE_HIDE=mileage,perf,duration` |
| Debug timing | `src/config.rs` :: `timed` | `STATUSLINE_DEBUG_TIMING=1` prints per-segment ms to stderr |
| Typed input schema | `src/input.rs` :: `StatusInput` | serde structs with `Option<T>` everywhere; tolerates CC schema drift |

---

## 🎯 Planned (next up)

Highest-signal ideas from the brainstorm, ranked roughly by impact-per-effort.
Refreshed 2026-05-21 after auditing 18+ competitor projects; the items here
are the ones either novel to us or convergently validated across peers.

1. **OAuth `/api/oauth/usage` for canonical 5h/7d** — Two competitors
   (`CCometixLine`, `claude-code-mascot`) independently read
   `~/.claude/.credentials.json` / macOS Keychain and call Anthropic's own
   `/api/oauth/usage` endpoint with `anthropic-beta: oauth-2025-04-20`.
   Returns real `{five_hour: utilization, resets_at}` etc — solves the
   "cost is notional for subscribers" problem from CLAUDE.md. Mirrors the
   detached-fetch + 5min cache pattern from `anthropic.rs` / `aggregate.rs`.
   _Risk: undocumented `anthropic-beta` header; ToS-adjacent (Anthropic's
   own client uses it, so it's likely fine — but worth flagging in docs)._

2. **Active git operation** (`MERGE`/`REBASE`/`CHERRY-PICK`) — Single call
   to `git2::Repository::state()`; renders as a badge next to branch. Load-
   bearing during conflict resolution. Trivial — maybe 30 lines.

3. **Untested code Δ** — `untested +247` (git diff lines in `src/` without
   matching test changes). Cumulative "you owe tests" debt for the session.
   _Data: `git diff --stat` two passes, one filtered to source/, one to test
   patterns; subtract._ Genuinely novel — none of the 18+ competitors ship this.

4. **Time since last commit** — `nocommit 47m`. The "save your work" nag.
   _Data: `git log -1 --format=%ct` mtime delta. Effectively free._

5. **Compact counter** — `cmp:3` (number of auto-compactions this session).
   After 3-4 compactions model quality drifts; surface it as hard data.
   _Data: scan transcript JSONL for compaction event markers._

6. **Auto-compact distance** — `cmp in 12%` (replaces `ctx 78%` framing
   above ~70%). Distance-to-event vs absolute usage; more actionable.
   _Data: ctx % math; just a re-frame of the existing segment._

7. **Burn acceleration** — `$24/h ↑` when last 10 min is N× the session
   average. Catches runaway agents in real time, not after the bill arrives.
   _Data: split cost.total_cost_usd against transcript timestamps to derive
   recent burn vs trailing average._

8. **Block cost projection** — `proj $14 by reset` next to the 5h block.
   `current + (burn_rate × minutes_remaining)`. Cleaner than 7d-pace
   projection because bounded by the 5h window. Validated by `ccusage`
   and `rz1989s/claude-code-statusline`.

9. **Context-fill ETA** — `eta:15m` (until `/compact`). Velocity from a
   disk-persisted ring buffer of `(ctx_pct, ts_ms)` samples. Codachi shipped
   the formula but with an in-memory buffer (always returns 0); we'd do it
   correctly with disk persistence.

---

## 🗂 Backlog

Live, plausible ideas — not next-up but worth keeping warm.

### Safety / signals

- **MCP health badges** — `mcp serena✗ ctx7✓` (only failing servers shown).
  The silent serena disconnect that wrecks your session. _Caveat: must avoid
  subprocess-per-render overhead — only fire on a cached interval._
- **Service tier drop** — `tier:standard↓` only when fallen from `priority`.
  Surfaces when Anthropic is busy and your stream is slower than usual.
- **Cancel-impact warning** — `ctrl-c: -3wt -180LOC` (in worktree with
  uncommitted work). "What do I lose if I bail right now?"
- **Idle threshold warning** — `IDLE 8m` only when no user msg in >5min
  during a high-cost burn. The "walked away while an agent burned $40" catcher.
- **Off-peak promo indicator** (`⬆` next to 5h) — Detects active Anthropic
  usage-promotion windows where 5h limit is boosted and excess doesn't count
  toward 7d. Directly actionable (work harder during off-peak). From
  `lexfrei/claudeline`.
- **Conflicts glyph** (`⚠`) — Distinct from "dirty" — files in `UU`/`AA`/`DD`
  state block all forward motion. Three competitors carry this as a separate
  state from generic dirty.

### Productivity / meta

- **Plan burndown** — `plan 4/12` when using the Plan tool. _Requires a way
  to detect "active plan" from the session._
- **Vibe score** — `vibe 73/100` composite (cost efficiency × low compactions
  × tests-keeping-up × commit cadence). Single gamified number.
- **Personal records** — longest / priciest session badge (top 3). Subtle
  awareness when breaking your own records.
- **Same-repo active sessions** — `cc:3` other CC processes in the same repo.
  "You forgot a tab somewhere."
- **Per-session deterministic color hash** — `hash(session_id) → palette[20]`
  so the same session is always the same color across tmux panes. From
  `jbarbier/which-claude-code`. Trivial — pairs naturally with our worktree
  segment for multi-pane visual distinguishability.
- **Hook-derived booleans** (struggling / rapid-edit / exploring) — A
  `PostToolUse` hook writes a 50-entry rolling event log; segment derives
  `fail:3↑` (consecutive bash failures), `edit:7/60s` (rapid edits), or
  `read:9/60s` (exploring). Requires a hook subsystem (contradicts pure-stdin
  design) but high signal. From `codachi`.

### External world

- **Linear / Jira current ticket** — `BNK-247` cached 5min via CLI.
- **Calendar next event** — `mtg in 14m` from `gcalcli`. Hard interrupt warning.
- **GitHub reviews-on-me** — `pr-reviews:3` from `gh search prs
  --review-requested=@me`. "You're heads-down too hard."

### Niche / nerdy

- **Hook trigger frequency** — `hooks 142/turn`. Catches misconfigured hooks.
- **Background shell count** — `bg:3` for `run_in_background` shells still
  alive. Orphan detector.
- **Cache tier mix** — `5m:80% 1h:15%` between Anthropic's two cache tiers.
- **Day streak** — `streak:14d` Duolingo-for-terminals.
- **Hijri date** — `15 Dhul-Qa'dah` instead of weather. Locally meaningful.
- **Session-cost-as-meme** — `$1010 ≈ 2.5× Netflix Standard for a year`,
  swappable comparators (iCloud / Spotify / your AWS bill / a flight to Dahab).
- **ASCII yak mascot** — character grows shaggier as yak shave depth
  increases. Currently we render `yak~~~:4` (more tildes); a real glyph
  would push this further.

---

## 🚫 Rejected (and why)

- **Weather (wttr.in)** — meme noise, no agent-workflow signal.
- **Bitcoin price** — same.
- **Wordle streak** — same.
- **Multi-line statusline** — breaks the "glance" model. Statusline must be
  scannable in <300ms.
- **Sparklines per-render** — visual noise; the eye chases the bar instead
  of the numbers. The boss-fight gradient already serves this purpose.
- **Real-time MCP health (subprocess-per-render)** — overhead too high. A
  cached variant is still in [Backlog](#safety--signals).
- **Pet/mascot reacting to hooks** — Three TS projects converged on this
  (`ccpet`, `codachi`, `claude-code-mascot`). The underlying *signals* they
  derive (hook-driven booleans, context ETA) are interesting; the visual
  layer is not. We extract the signals into our own segments — see backlog
  entries — and skip the affective layer.
- **Daemon mode** — `claude-statusbar`'s sub-1% CPU daemon solves a Python
  startup problem we don't have. We're 6ms total; daemon-fication is
  unwarranted complexity. Mtime-filtered file scan + atomic-rename cache
  (see `aggregate.rs`) gets the same effect without a daemon.

---

## Implementation notes

### Data sources by category

- **Statusline JSON (`data.*`):** model, workspace, context_window, cost,
  effort, thinking, fast_mode, output_style, rate_limits, version,
  session_id, transcript_path, pr, worktree.
- **Transcript JSONL (tail-read):** timestamps, output_tokens per turn,
  `sourceToolAssistantUUID`, `isSidechain`, tool_use blocks (destruction
  count, yak depth, last-turn rate).
- **Filesystem (`fs::metadata`):** worktree HEAD mtimes (stale check),
  `.git/HEAD` read for branch detection, settings.json (plugins),
  cached Anthropic status.
- **libgit2 (via `git2` crate, statically linked):** dirty/stash/ahead-behind,
  worktree enumeration, diff for TODO delta. No subprocess overhead — direct
  C library calls from the binary.
- **External HTTP (cached):** `status.claude.com/api/v2/status.json` via a
  detached background `curl`; result picked up by the next render's
  reconcile step. No blocking on network in the render hot path.
- **Derived math:** burn rate, tok/s, $/LOC, mileage, pace projection, FTL.

### Refresh cadence

| Source | Cache window |
|---|---|
| Anthropic status | 5 min |
| GitHub `gh` queries (when shipped) | 60 sec |
| Worktree mtime checks | per-render (cheap) |
| settings.json reads | per-render (cheap) |
| Transcript JSONL tail | per-render (cheap if <500KB) |

### Performance budget

CC statusline soft-timeout ≈ 1 second. We target an order of magnitude
under that — current renders are **~6 ms in-repo, ~1.5 ms no-repo** on
Apple Silicon, measured via `STATUSLINE_DEBUG_TIMING=1`.

| Layer | Cost | Notes |
|---|---|---|
| libgit2 status + ahead/behind + stash | ~2 ms | Single Repository::statuses pass |
| libgit2 diff for TODO delta | ~2 ms | Skipped when working tree is clean |
| libgit2 worktree enumeration | ~0.1 ms | Already in memory after Repository open |
| `fs::metadata` for Anthropic status cache | ~1.5 ms | Single stat call |
| `.git/HEAD` read for branch | ~10 µs | Reads ~40 bytes |
| `find_gitdir` walk-up | ~10 µs | Dir-stat per ancestor, usually 1-3 levels |
| Transcript JSONL tail (256 KB) | ~50 µs | Single tail read + serde parsing |
| ANSI rendering, formatters, all string ops | <1 ms total | Pure compute |

The pre-Rust Node version was ~105 ms. The first Rust port was ~40 ms.
Phase 1 (filesystem fast path + parallel subprocesses) cut to ~20 ms.
Phase 2 (libgit2) cut to ~6 ms. Beyond this, gains would require things
like (a) caching libgit2's Repository handle across renders (impossible
without a daemon — each render is a fresh process) or (b) skipping the
Anthropic status check entirely.

### Optimization journey (Phase 1 → Phase 2)

For curious readers: the commits at `813abca` (Phase 1, parallel-subprocess
+ `.git/HEAD` filesystem reads) and `371cf7c` (Phase 2, libgit2) show the
two distinct optimization moves with measured before/after numbers in their
commit messages. Useful reference for understanding what each layer costs.

---

_Curated from session 2026-05-20 with Ahmed @ Intercom Enterprises
(banknet2-retail rewrite)._
