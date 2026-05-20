# CLAUDE.md — claude-code-statusline

Rust CLI that reads a JSON-on-stdin contract from Claude Code and emits a
single rendered status line to stdout. Runs once per CC render.

For end-user docs and install, see README.md.
For shipped segments + backlog, see docs/ROADMAP.md.

## Commands

```bash
cargo build --release          # ~30s first time (libgit2 vendored), then incremental
cargo test --release           # 47 tests; runs in <1s
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
  `/tmp/cc-statusline-input.json`. Use this to detect schema drift
  the next time CC changes a field's location or semantics.

## Performance notes

- Render budget: **~6ms in-repo, ~1.5ms no-repo** on Apple Silicon.
- 98% of the in-repo budget is libgit2 calls (status / diff / worktree).
  Pure Rust compute is sub-millisecond.
- `find_gitdir` walks up looking for `.git` before any libgit2 call.
  When absent, ALL git work is skipped — that's the no-repo 1.5ms case.
