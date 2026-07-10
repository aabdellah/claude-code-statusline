# CLAUDE.md — claude-code-statusline

Rust CLI that reads a JSON-on-stdin contract from Claude Code and emits a
single rendered status line to stdout. Runs once per CC render.

For end-user docs and install, see README.md.
For shipped segments + backlog, see docs/ROADMAP.md.

## Commands

```bash
cargo build --release          # ~30s first time (libgit2 vendored), then incremental
cargo test --release           # 120+ tests; runs in <1s
cargo run -q                   # local invocation; pipe JSON to stdin

# After source edits if the LaunchAgent isn't running:
cargo build --release

# Live-update install:
./install.sh                   # idempotent — also the update path
./install.sh --uninstall
```

## Architecture (only what's non-obvious)

- **`src/segments/`** — one file per status-line block. Each exposes
  `pub fn render(ctx: &RenderContext) -> Option<Seg>`. Returning `None`
  hides the segment. To add one: drop a file in `src/segments/`, add
  `pub mod foo;` to `mod.rs`, insert `foo::render` in `FUNCS`. No other
  edits anywhere.

- **`src/context.rs`** — `RenderContext::build` does ALL I/O up-front
  (git status, transcript read, etc.) so segment functions stay pure.

- **`src/layout.rs`** — adaptive width fitter. Each `Seg` has up to three
  variants (full / compact / micro) and a `Priority`. The fitter downgrades
  lowest-priority segments until the line fits the terminal width.
  `Critical` segments never drop.

- **`src/repr.rs`** — enforces label↔value formatting consistency. New
  segments MUST use these helpers for the canonical shapes:
    counter         "label:N"           full / "label_short:N"   compact
    percent         "label N%"          full / "label_short:N"   compact
    signed_delta    "label +N" / "-N"   full / "label_short:+N"  compact
    labeled_status  "label:value"       full / "label_s:val_s"   compact
  Glyph-prefixed counts (●3, ↑5, #247) and atomic values ($4.21, 47m,
  model names) DON'T use repr — they're inline by design.

## Gotchas (the schema-drift bugs and friends)

- **`exceeds_200k_tokens` is at the TOP LEVEL of CC's input JSON**, not
  nested under `context_window`. Nesting it inside `ContextWindow` made
  the 200k+ warning silently never fire for the whole life of the Node
  version and early Rust versions. The field belongs on `StatusInput`.

- **`context_window.total_input_tokens` / `total_output_tokens` are
  CURRENT-WINDOW snapshots since CC v2.1.132**, not cumulative session
  totals. Anything that divides cumulative lines by these will produce
  nonsense. Use `cost.total_api_duration_ms` for cumulative-API-time
  productivity instead.

- **Fable 5's dedicated limit is the `seven_day_overage_included` window —
  and CC does NOT forward it to the statusline yet.** Verified against the
  v2.1.201 binary (strings + minified JS): CC parses four windows from the
  `anthropic-ratelimit-unified-{5h,7d,7d_oi,overage}-utilization/-reset`
  response headers; `7d_oi` = `seven_day_overage_included`, UI label
  "Fable 5 limit" ("overage included" because Fable usage can spill into
  usage credits). The statusline payload builder whitelists ONLY
  `five_hour` and `seven_day` — the Fable window never reaches stdin as of
  2.1.201, even though it sits in the same live header-parsed state the
  payload is built from (one whitelist line away). `seven_day_opus` /
  `seven_day_sonnet` are a step further out: CC never parses those from
  headers at all — they exist only as rateLimitType enum values and as
  oauth-usage fields, so our key names for them are guesses. We parse +
  render all three anyway (drift-proof: each window deserializes leniently,
  so a wrong-shaped key nulls only itself).
  Until CC forwards them, `src/usage.rs` BRIDGES the gap: the detached
  refresh process fetches the oauth usage endpoint and caches the scoped
  windows; the segment falls back to that cache for windows stdin doesn't
  carry (stdin always wins when present). If the fable segment is missing,
  check `/tmp/cc-statusline-usage.json` freshness and a
  `STATUSLINE_DUMP_INPUT=1` dump before touching parsing — the data
  genuinely isn't in the stdin input. The richer
  source CC itself uses for `/usage` is `GET /api/oauth/usage` (OAuth).
  Its response is FLAT — no `rate_limits` envelope (that's CC's internal
  aggregate name, not the wire shape): top-level `five_hour`, `seven_day`,
  `seven_day_opus`, `seven_day_sonnet`, `seven_day_oauth_apps`,
  `cinder_cove`, `extra_usage`, and `limits[]` entries of
  `{kind: "weekly_scoped", scope: {model: {display_name}}, percent, resets_at}`
  (the `/usage` panel renders five_hour + seven_day + seven_day_sonnet +
  the weekly_scoped limits).

- **Both OAuth credential stores can hold LIVE tokens for DIFFERENT
  accounts at once.** `/login` to another account rewrites only the store CC
  is configured for; the abandoned store's token stays unexpired for hours.
  Preferring the file whenever its token was unexpired made the fable limit
  render the PREVIOUS account's quota until that token died. Only CC
  refreshes tokens, and only in the active store — so the live token with
  the later `expiresAt` is the active account's. Any future credential read
  must use `usage::select_token`'s rule, never "file first".

- **`git2` MUST be `default-features = false, features = ["vendored-libgit2"]`.**
  Without `vendored-libgit2`, `git2-sys` finds brew's system libgit2 via
  pkg-config and dynamic-links to it, breaking the "self-contained
  binary" claim across machines.

- **`tmux display -p '#{pane_width}'` needs `-t $TMUX_PANE`.** Without
  the target flag, tmux returns the focused pane's width — which is
  often a sibling pane, not the one CC is running in.

- **CC frame consumes 2 cells on each side of its chat pane.** Use
  `Config::width_margin` (default 4) to subtract before fitting.
  Without this the rendered line overflows the visible area by 1-3 chars.

- **`yak_depth` reads the LATEST transcript entry, not the latest
  sidechain entry.** Walking back through history to find any sidechain
  was the cause of "yak:1 always" bug in early versions. Latest entry's
  `sourceToolAssistantUUID` chain only.

- **CC renders the statusline ONLY at turn boundaries (between user
  prompts), not mid-turn.** By the time a render happens, the assistant
  has finished its response and the latest transcript entry is back in
  the main thread. Implication: "current state" metrics rarely fire in
  practice — design new signals as "max this session" / "in last N min"
  rather than "right now" for them to actually be visible.

- **Fresh / worktree sessions send near-empty JSON until first model
  interaction.** Only `workspace.*` and `worktree.*` are populated; model,
  context_window, cost, rate_limits all arrive after the first turn. The
  minimum-data rendering (just "claude · repo/branch") is expected, not a
  bug. Each segment's `Option::None` return is what enables this.

- **For subscribers, `cost.total_cost_usd` is NOTIONAL** (what this
  session would cost at API rates) — not actual billing. Subscribers pay
  a flat monthly fee; their real budget is the 5h/7d rate-limit windows.
  Tokens over 200k count ~2x against subscription quotas, not against $.
  CC doesn't differentiate subscribers from API users in the JSON, so the
  same field has different meaning depending on the user.

- **`thinking.enabled` is redundant with effort level — we don't render
  it as a separate word.** CC's `/effort` controls both: effort >= "high"
  implies thinking is on; below that it's off. So `effort: max` and
  `thinking: true` always come together in practice. We keep
  `input.thinking` parsed (forward-compat in case CC decouples them) but
  only render the effort word. Don't re-add a "thinking" indicator
  without verifying the fields can actually disagree.

- **`workspace.git_worktree` is a STRING (worktree name), not a bool.**
  CC v2.1.145+ ships it as `"git_worktree": "feat-x"`. Earlier versions
  sent a bool. When this struct typed it as `Option<bool>`, serde failed
  on the whole `Workspace`, and (because of the old all-or-nothing
  `parse_lenient`) the WHOLE `StatusInput` defaulted to empty — collapsing
  every worktree statusline to `claude · branch` with no metrics. Fixed by
  retyping AND switching `parse_lenient` to field-by-field so one drift
  can never blank everything again. New top-level fields added to CC's
  contract require updates to both the struct AND the `field(obj, "...")`
  call list in `parse_lenient`.

## Conventions

- **Test ANSI-bearing output by stripping escapes first** —
  `compact.contains(":+4")` will fail when there's a color code between
  the `:` and `+`. See `src/repr.rs` tests for the stripping pattern.

- **Never use byte-slicing on unsanitized strings** — `s[..3]` will
  panic on multi-byte UTF-8. Use `s.chars().take(3).collect()`.

- **`cargo build --release` overwrites the binary in place** — the
  `~/.claude/bin/cc-statusline` symlink points at the build output,
  so a rebuild is a live deploy. The LaunchAgent (`--with-autobuild`)
  triggers `cargo build --release` on source edits via `cargo-watch`.

- **STATUSLINE_DUMP_INPUT=1** writes the raw stdin JSON to
  `/tmp/cc-statusline-input.json` AND to
  `/tmp/cc-statusline-dumps/<session_id>.json`. The per-session path
  matters: the single-file path is shared across every concurrent CC
  session on the machine, so race-overwrites give false negatives when
  investigating one session's render. Always use the per-session file when
  diagnosing a specific session.

- **`STATUSLINE_USAGE_SOURCE=off`** disables the oauth usage fetch AND its
  rendering (the Fable dedicated weekly sourced from `GET /api/oauth/usage`).
  Note the value is `off`, not pricing's `offline` — the semantics differ:
  `off` kills fetch *and* the cache read/render; pricing's `offline` only
  stops new network calls (a prior cache is still consulted). The fetch runs
  in the detached refresh process only: OAuth token from whichever of
  `~/.claude/.credentials.json` / macOS keychain item "Claude Code-credentials"
  holds the live token with the LATER `expiresAt` (see the two-store gotcha
  below), validated against the
  JWT/base64url charset, then passed to curl via stdin config — NEVER argv
  (`ps` leaks argv to every local process) — and never written to disk.
  `curl`/`security` are invoked by absolute path (no PATH hijack). State
  (cache of scoped windows — usage % + reset times — and the retry marker)
  lives in a PER-USER private dir (`$TMPDIR`, else `~/.cache/cc-statusline`
  at 0700), 0600 files, NOT world-writable `/tmp`: the data is
  account-specific, so a shared path would leak/spoof across users and
  expose fixed paths to symlink-clobber. 120s TTL, 10min failed-attempt
  throttle, 15min max render age; cache AND retry marker are stamped with
  `oauthAccount.accountUuid` (from `~/.claude.json`) and a stamp mismatch
  voids both, so a `/login` refetches immediately as the new account.
  Expired tokens are skipped, not refreshed
  — CC rewrites the credentials file as it refreshes, and the next cycle
  picks the new token up.

- **`STATUSLINE_PRICING_SOURCE=offline`** disables the LiteLLM pricing
  fetch and forces the embedded Claude-family constants (Fable/Opus/Sonnet/
  Haiku) for the
  cross-session today rollup. The cache file (`/tmp/cc-statusline-pricing.json`)
  is still consulted if present — "offline" means "don't make new network
  calls," not "ignore any prior cache." For airgapped builds also pass
  this and `rm` the cache file. Live fetch is from
  `raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json`
  with 24h TTL and a 1h failed-attempt throttle. Filters by
  `litellm_provider == "anthropic"`; unknown models fall back to family
  substring match (fable/mythos/opus/sonnet/haiku), then to `None` (tokens still
  counted, $ skipped).

- **Embedded pricing constants drift fast.** When live pricing first
  landed (2026-05-21), the embedded Opus rates were ~3× current Anthropic
  prices because Opus 4.7 dropped to $5/$25/M (input/output) but the
  embedded numbers were still at Opus 4.0-era $15/$75/M. Live LiteLLM
  data is the correct default; embedded is the fallback for unreachable
  LiteLLM. Don't trust the embedded numbers as ground truth — they're a
  graceful-degradation floor only.

## Performance notes

- Render budget: **~6ms in-repo, ~1.5ms no-repo** on Apple Silicon.
- 98% of the in-repo budget is libgit2 calls (status / diff / worktree).
  Pure Rust compute is sub-millisecond.
- `find_gitdir` walks up looking for `.git` before any libgit2 call.
  When absent, ALL git work is skipped — that's the no-repo 1.5ms case.
