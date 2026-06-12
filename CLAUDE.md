# trusty-tools — Claude Code Instructions

Unified Rust workspace consolidating the entire trusty-* AI tooling ecosystem.
20 crates — shared libraries, daemon/MCP servers, the MPM platform, the
control plane, and an orchestrator — all co-located under one Cargo workspace.

## Project Overview

This is a **Rust workspace** (Cargo workspace, resolver v2, glob members
`crates/*`) under the Elastic License 2.0 (per-crate; a few crates are MIT —
see each `Cargo.toml`). Every crate manages its own `version` field independently;
`[workspace.package]` shares `rust-version`, `edition`, `license`, `repository`,
and `authors` but no longer carries a version field (see #343).

**MSRV**: `1.91` — driven by indirect `aws-smithy-*` dependencies that declare
`rust-version = "1.91.1"`; the let-chain stabilisation floor (1.88) is lower.
CI enforces this with `dtolnay/rust-toolchain@1.91`.

## Role & Scope

`trusty-tools` is the **single source of truth** for all trusty-* AI tooling.
It replaces seven formerly separate repos and eliminates the `[patch.crates-io]`
dance for cross-crate development. The authoritative crate list is the
`[workspace.members]` glob in the root `Cargo.toml`; every subdirectory under
`crates/` with a `Cargo.toml` is a member.

Key consumers of the shared libraries:
- **trusty-agents** — agent orchestration platform (lives in `crates/trusty-agents`)
- **trusty-search** — hybrid code search daemon + MCP server
- **trusty-memory** — MCP frontend over the memory palace (storage lives in
  `trusty-common`'s `memory-core` feature)
- **trusty-analyze** — code analysis daemon (complexity, smells, quality metrics)

Work touching a shared crate (e.g. `trusty-common`, `trusty-embedderd`) may
require bumping the dependent crate's version and verifying its tests. Always
run `cargo check` and `cargo test -p <crate>` after modifying a library crate,
then propagate changes to all crates that depend on it before committing.

## Build and Test Commands

🔴 **Single-path workflows — use exactly these commands.**

### Workspace-wide Commands

```bash
# Build all crates (development)
cargo build

# Build all crates (release/optimised)
cargo build --release

# Run all tests
cargo test

# Check (fast compile check, no codegen)
cargo check

# Lint (workspace-wide, all targets)
cargo clippy --workspace --all-targets -- -D warnings

# Format check
cargo fmt --check

# Format and fix
cargo fmt

# Run ONNX-backed integration tests (slow; skipped in CI)
cargo test -- --include-ignored

# Update dependencies (review Cargo.lock diff before committing)
cargo update

# Audit dependencies for known vulnerabilities
cargo audit   # requires: cargo install cargo-audit
```

### Individual Crate Commands

```bash
# Build a single crate (dev)
cargo build -p trusty-search

# Build a single crate (release)
cargo build --release -p trusty-search

# Check a single crate (fastest — no codegen)
cargo check -p trusty-search

# Test a single crate
cargo test -p trusty-search

# Test a single crate with a specific feature
cargo test -p trusty-common --features axum-server

# Test a single test by name within a crate
cargo test -p trusty-search -- my_test_name

# Run a binary from a specific crate
cargo run -p trusty-search -- start
cargo run -p trusty-mpm -- --help

# Build only the binary, not the whole workspace
cargo build --release -p trusty-mpm

# Lint a single crate
cargo clippy -p trusty-search -- -D warnings

# Test a single crate with ignored tests
cargo test -p trusty-embedderd -- --include-ignored

# Run trusty-search performance regression suite (requires daemon + indexed trusty-tools)
cargo test -p trusty-search --test baseline_trusty_tools -- --include-ignored --nocapture
```

### Important: Crate Names vs. Directory Names

**Crate names** match the `name` field in each crate's `Cargo.toml`, not necessarily the directory name.
Most match (e.g. `crates/trusty-search/` → `-p trusty-search`) but note these exceptions:

- `crates/trusty-git-analytics/` → `-p tga` (short name)
- `crates/trusty-agents/` → `-p trusty-agents`

Always verify the `name` field in the crate's `Cargo.toml` if you get a "package not found" error.

## Code Structure

```
trusty-tools/               # workspace root
├── Cargo.toml              # workspace manifest — glob members = ["crates/*"]
├── Cargo.lock
├── crates/                 # 20 members (matches `ls crates/`)
│   ├── trusty-common/       # shared utilities, tracing, OpenRouter chat; hosts the
│   │                        # consolidated mcp/rpc/embedder/symgraph/memory-core/
│   │                        # tickets/monitor-tui modules behind feature flags
│   ├── trusty-embedderd/    # fastembed wrapper — sidecar daemon for trusty-search
│   ├── trusty-bm25-daemon/  # BM25 index daemon — sidecar for trusty-memory
│   ├── trusty-gworkspace/   # Google Workspace client (Calendar, Tasks, Drive)
│   ├── trusty-cto-db/       # SQLite CTO database (rusqlite-backed)
│   ├── tc-services/         # service-layer adapters: CTO DB, Granola, GWorkspace
│   ├── trusty-search/       # hybrid BM25 + vector + KG search daemon + MCP server
│   ├── trusty-memory/       # MCP server frontend for memory (includes Svelte UI)
│   ├── trusty-analyze/      # code analysis daemon + MCP server
│   ├── trusty-mpm/          # unified MPM platform: CLI (tm/trusty-mpm), daemon, MCP, TUI, Telegram
│   ├── trusty-mpm-gui/      # MPM desktop GUI (Tauri, publish=false)
│   ├── cto-assistant/       # CTO assistant CLI (publish=false)
│   ├── trusty-git-analytics/ # developer productivity analytics (tga)
│   ├── trusty-agents/       # agent orchestration platform (publish=false)
│   ├── trusty-agents-common/ # trusty-agents common API types (publish=false)
│   ├── trusty-agents-local/ # trusty-agents local execution (publish=false)
│   ├── trusty-code/         # per-project Claude-Code-compatible MPM orchestration harness (bin: tcode); Phase 0 scaffold; extraction tracked in #587
│   └── trusty-controller/   # thin control plane for the claude-mpm stack (bin: tctl); Phase 0 scaffold; RFC tracked in #920
└── .gitignore
```

> **Consolidation note:** the formerly separate `trusty-symgraph`, `trusty-rpc`,
> `trusty-tickets`, `trusty-mcp-core`, `trusty-embedder`, `trusty-memory-core`,
> and `trusty-monitor-tui` crates no longer exist as standalone directories —
> they were absorbed into `trusty-common` behind the `symgraph`, `rpc`,
> `tickets`, `mcp`, `embedder`, `memory-core`, and `monitor-tui` feature flags
> respectively. Enable the relevant feature to pull in the corresponding module.

For the source layout of any crate, read its `README.md` or browse
`crates/<name>/src/`. Each crate owns its own `README.md` covering purpose,
usage, and design notes.

## Documentation Layout

Documentation is organized by **published crate**, not by topic. Each crate gets
a directory under `docs/` containing three standard subdirectories:

```
docs/
├── trusty-search/              # See here for the worked example
│   ├── regression-testing/     # Versioned snapshots: v{VERSION}-{DATE}.md
│   ├── research/               # Investigation & decision docs: *-{DATE}.md or *-decision-{DATE}.md
│   └── sessions/               # Engineering session summaries: SESSION-{DATE}-{topic}.md
├── trusty-memory/              # Follows the same three-subdir convention
├── trusty-common/              # (and all other published crates)
├── trusty-mpm/                 # covers all 8 trusty-mpm-* binaries
├── trusty-agents/
├── trusty-analyze/
└── trusty-git-analytics/
```

**Purpose of each subdir**:
- **`regression-testing/`** — Performance snapshots tied to releases. One `.md`
  file per measured release named `v{VERSION}-{YYYY-MM-DD}.md`; alternate-corpus
  baselines (e.g., synthetic, trusty-agents) live alongside; `current.md` is a
  symlink to the latest snapshot.
- **`research/`** — Investigation outcomes, audits, decision documents. Named
  `{topic}-{YYYY-MM-DD}.md` or `{topic}-decision-{YYYY-MM-DD}.md`.
- **`sessions/`** — Engineering-session narratives. Named
  `SESSION-{YYYY-MM-DD}-{topic}.md`.

Each subdir has a `README.md` explaining its purpose, file naming, and indexing
conventions. **See `docs/trusty-search/` as the authoritative worked example.**

For **cross-release performance tracking**, see GitHub issue
[#129](https://github.com/bobmatnyc/trusty-tools/issues/129): it accumulates
benchmark deltas across all measured versions.

## Key Conventions

🔴 **Why/What/Test doc pattern** — every public item (function, struct, trait,
module) carries three comment sections:

```rust
/// Why: <motivation — the problem this solves, not the mechanics>
/// What: <mechanical description of what the item does>
/// Test: <where coverage lives, or why it is side-effect-only / untestable>
pub fn my_function() { … }
```

Never omit this pattern on public items. It is the primary way future readers
understand design intent without reading git history.

🔴 **No `unwrap()` in library code** — use `?` with `anyhow::Result` for
application/binary code and `thiserror` for library error types. Reserve
`expect()` only for cases that are genuinely programmer errors (invariants that
can never occur at runtime).

🔴 **SLOC file size hard cap (MECHANICALLY ENFORCED, dual-cap since #1131):**

| File type | SLOC cap |
|---|---|
| Production source files | **500 SLOC** |
| Test / benchmark files | **1500 SLOC** |

A file is classified as a **test/benchmark file** when ANY of these match:
- basename is exactly `tests.rs`
- basename ends with `_test.rs` or `_tests.rs`
- path contains a `/tests/` directory segment (covers `crates/*/tests/*.rs`
  integration tests AND any `src/**/tests/*.rs` inline test modules)
- path contains a `/benches/` directory segment

All other tracked `.rs` files are **production files**, capped at 500 SLOC.

As of issue #610 the production cap is no longer advice: it is gated by
`scripts/check_line_cap.sh`, wired into CI (`.github/workflows/line-cap.yml`)
and the local pre-commit hook (`line-cap`). A new tracked production `.rs` file
over 500 SLOC **cannot merge**; a new test/benchmark `.rs` file over 1500 SLOC
**cannot merge**. Files approaching their limit are a signal to split into
focused submodules before the next feature lands on them. When splitting, prefer:
one public module per logical concept, a thin `mod.rs` that re-exports, and
sibling files with clear single responsibilities.

**SLOC definition:** a line counts only when it contains non-whitespace source
code after all comment matter is stripped. These are **excluded** from the count:
- blank / whitespace-only lines
- `//` line comments (including `///` doc comments and `//!` inner-doc comments)
- `/* ... */` block comments — including multi-line spans; every line inside an
  open block comment is excluded
- lines that consist entirely of a closing `*/`

A line that has code followed by a trailing `// comment` **still counts** — it
has code. The counter is a pragmatic awk heuristic that errs toward leniency:
edge cases (e.g. `//` inside a string literal) may undercount SLOC but will
never falsely fail a legitimate file.

**The ratchet (allowlist that can only shrink):** grandfathered files over their
applicable cap are listed in `.line-cap-allowlist.tsv` (one
`relative/path<TAB>budget` line each, where `budget` is that file's frozen max
SLOC count). The gate enforces per-applicable-cap ratchet semantics:

- SLOC ≤ applicable cap and **not** allowlisted → OK;
- SLOC > applicable cap and **not** allowlisted → **FAIL** (new oversized file — split it);
- allowlisted, current SLOC **exceeds its budget** → **FAIL** (it grew — split it);
- allowlisted, current SLOC **≤ applicable cap** → **FAIL** (drop the entry; ratchet-down forcing function);
- allowlisted, `applicable_cap < SLOC ≤ budget` → OK (grandfathered, not growing).

So allowlisted files may only shrink, and no new oversized file may be added.
As the #607 sweep and per-crate refactors land, the allowlist ratchets down
toward empty.

**Run it locally:** `bash scripts/check_line_cap.sh` (exit 0 = clean). After you
intentionally split a file (or a file otherwise drops below its budget), refresh
the frozen budgets with `scripts/check_line_cap.sh --update` — this only *lowers*
budgets or *removes* entries that fell ≤ their applicable cap; it **refuses** to
add a new oversized file or raise a budget unless you pass `--seed` (initial
bootstrap) or `--force-add` (rare, intentional bump). Commit the regenerated
`.line-cap-allowlist.tsv` alongside your split.

Past violations (refactor tickets #170/#171/#172 are CLOSED and the splits have
landed — all three former monoliths are now under the 500-SLOC cap):
- `crates/trusty-agents/src/ctrl/mod.rs` — RESOLVED (#170). Split into focused
  submodules under `crates/trusty-agents/src/ctrl/` (`state`, `config`, `repl`,
  `handlers`, `pm_task`, …); `mod.rs` is now a ~50-line re-export facade.
- `crates/trusty-agents/src/runtime/` — RESOLVED (#171). The original `runtime.rs`
  was split into a `runtime/` module; every submodule is now under the cap.
- `crates/trusty-agents/src/workflow/engine/` — RESOLVED (#172). The original
  `engine.rs` was split into an `engine/` module; every submodule is now under
  the cap (largest is `engine/executor/run.rs` at ~485 lines).

The largest remaining production file in `trusty-agents` (not tied to an open
ticket) is `tm/manager.rs` — file a fresh refactor ticket before growing it
further. Current per-file SLOC budgets live in `.line-cap-allowlist.tsv`.

🔴 **`thiserror` for libraries, `anyhow` for binaries** — library crates
(`trusty-common`, `trusty-embedderd`, `trusty-bm25-daemon`, etc.) define structured error enums with
`#[derive(thiserror::Error)]`. Binary and daemon crates use `anyhow::Result`
throughout.

🔴 **Feature flags** — `trusty-common` gates `axum` and `tower-http` behind the
`axum-server` feature flag. Do not add axum as an unconditional dependency in
any library crate. Enable it explicitly in crates that serve HTTP.

🟡 **Rust editions** — `edition = "2024"` for `trusty-mpm`, `trusty-mpm-gui`, `trusty-agents`, `trusty-agents-common`, and `trusty-agents-local`
(they use let-chains); `edition = "2021"` for all other crates. Check the crate
`Cargo.toml` before assuming an edition.

🟡 **No global state** — all helpers are free functions or small structs. No
`lazy_static!` or `once_cell::sync::Lazy` except the tracing subscriber (which
uses `try_init` to be idempotent across test binaries).

🟡 **Logs to stderr** — `init_tracing` always writes to stderr so stdout stays
clean for MCP JSON-RPC framing. Never log to stdout in a daemon or MCP server.

🟡 **Ignore-tagged integration tests** — ONNX-backed embedder tests are marked
`#[ignore]` to keep CI fast. Run with `cargo test -- --include-ignored` when
you need local validation against the model.

🟢 **Workspace dependency sharing** — all shared external crates (`anyhow`,
`serde`, `tokio`, `axum`, etc.) are declared once in `[workspace.dependencies]`
in the root `Cargo.toml` and referenced as `dep = { workspace = true }` in
crate manifests. Never pin a dependency locally if it is already in the
workspace table.

🟢 **Internal path deps** — reference sibling crates as:
```toml
trusty-common = { workspace = true }
```
The workspace manifest declares the path so every member resolves from the
in-tree source automatically. The `[patch.crates-io]` block in the root
`Cargo.toml` also redirects any crates that still reference the old published
versions by version number.

## Git Tag / Release Convention

Each crate is tagged independently using the pattern `<crate-name>-v<version>`,
e.g. `trusty-mcp-core-v0.2.0`. The version comes from the crate's `Cargo.toml`.

Release workflow:
1. Bump the crate version in `crates/<name>/Cargo.toml`.
2. Update any dependent crates that pin that version.
3. Run `cargo test -p <name>` and `cargo clippy --workspace -- -D warnings`.
4. Commit the version bump.
5. Create the tag: `git tag <crate-name>-v<version>`.
6. Push the tag: `git push origin <crate-name>-v<version>`.
7. Publish: `cargo publish -p <crate-name>`.
   - **UI-embedding crates** (trusty-search, trusty-memory, trusty-analyze): prefix with `SKIP_UI_BUILD=1`:
     ```bash
     SKIP_UI_BUILD=1 cargo publish -p <crate-name>
     ```
     The committed `ui-dist/` bundle is already in the repo; without this flag, `build.rs` will attempt to invoke `pnpm` inside cargo's verification tarball, which fails because it tries to modify files outside `OUT_DIR`.
8. Build the release binary (if not already fresh): `cargo build --release -p <crate-name>`.
9. Install the binary locally with `cargo install --path crates/<dir> --locked`
   (for crates with binaries, e.g. trusty-search, trusty-mpm). This ensures the
   binary on PATH is always the version that was just released.

   🔴 **Never `cp target/release/<binary> ~/.cargo/bin/<binary>` on macOS.**
   `cargo build` ad-hoc ("linker-signed") signs every release binary, and the
   kernel's code-signing cache is keyed by the executable's `cdhash`. A plain
   `cp` over an existing on-PATH binary can leave the kernel with a stale
   cached identity, so the next exec is SIGKILL'd with
   `EXC_CRASH / CODESIGNING — Taskgated Invalid Signature` **before any code
   runs** — the process dies with `zsh: killed` and zero output, which looks
   exactly like an OOM kill but is not. `cargo install` writes to a temp path
   and renames atomically, which keeps the cache consistent. If you must copy
   manually, follow it with `codesign --force --sign - ~/.cargo/bin/<binary>`
   to regenerate the ad-hoc signature against the final file.

Every crate manages its own version independently in its own `Cargo.toml`.
The `[workspace.package]` table no longer carries a `version` field (see #343).
When publishing, bump only the crates that actually changed — do not cascade
version bumps to siblings with no functional changes.

### macOS Full Disk Access must be re-granted after every `cargo install` (issue #873)

On macOS, every `cargo install` of a binary writes a NEW file at
`~/.cargo/bin/<binary>` with a new **cdhash** (code-signing hash). macOS TCC
keys the **Full Disk Access** grant by cdhash, so the previously-granted FDA no
longer applies to the freshly-installed binary. The launchd daemon then cannot
read indexes on `/Volumes/…` and warm-boot collapses from ~102 indexes to
**indexes:2** (only non-external-volume indexes load).

**After every `cargo install trusty-search` (or any binary that accesses
external/protected volumes as a launchd daemon), re-grant FDA:**

1. Open **System Settings → Privacy & Security → Full Disk Access**.
2. Remove `~/.cargo/bin/trusty-search` from the list.
3. Re-add it (`+` button, navigate to `~/.cargo/bin/trusty-search`).
4. Restart the daemon: `launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/<label>.plist && launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/<label>.plist`.

**Symptom:** `trusty-search status` shows `indexes:2` (or very few) immediately
after a reinstall, with warm-boot logs showing `skipped: blocked-volume` or
`tcc=57`. This is NOT data loss — all on-disk indexes are intact.

The daemon now detects this automatically: when the loaded count drops below 80%
of the prior-known count, `GET /health` returns `warm_boot_degraded: true` and
the daemon logs an error with the actionable FDA re-grant hint.

### Connection-safe daemon restart convention (issue #534)

As of trusty-common 0.10.0, all three HTTP daemons (trusty-memory, trusty-search,
trusty-analyze) implement graceful shutdown: they drain in-flight requests before
exiting when they receive SIGTERM. The `serve --stdio` proxy reconnects automatically
with exponential backoff when the daemon restarts (the `trusty-memory-mcp-bridge`
binary is a deprecated shim that forwards to `serve --stdio`; update your
`.mcp.json` to `"command": "trusty-memory", "args": ["serve", "--stdio"]`).

**Use `launchctl bootout` (SIGTERM), not `launchctl kickstart -k` (SIGKILL):**

```bash
# Graceful stop → install → restart
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/<label>.plist
cargo install --path crates/<dir> --locked
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/<label>.plist
# IMPORTANT on macOS: re-grant Full Disk Access after cargo install (see above)
```

Prefer restarting between Claude Code sessions. See the cargo-publish skill
(`.claude/skills/cargo-publish/SKILL.md`) for the full restart convention.

## Cross-Crate Development Workflow

Because all crates are in the same workspace, the `[patch.crates-io]` dance
that was required with separate repos is no longer necessary for active
development. Cargo resolves internal crates via path automatically.

When you modify a library crate:
1. Edit the crate under `crates/<lib>/`.
2. Run `cargo check` to catch compilation errors across the entire workspace.
3. Run `cargo test -p <lib>` for the modified library.
4. Run `cargo test -p <consumer>` for each crate that depends on the library.
5. Commit all changes together — workspace builds are atomic.

When publishing a crate to crates.io:
- The path dep in `[workspace.dependencies]` coexists with the version field,
  so `cargo publish` sees the version and uploads correctly.
- The `[patch.crates-io]` block in the root `Cargo.toml` ensures the in-tree
  crates are preferred during local builds even if a published version exists.

## Parallel Worktree Discipline

Multiple Claude Code sessions and subagents may share this repo concurrently.
The main checkout often holds another session's uncommitted work. To prevent
one session from stomping on another's edits, the following rules apply to
every session and every dispatched subagent.

🔴 **The main checkout is inspection-only.** From the repo root
(`/path/to/trusty-tools/` — wherever the repo lives on disk), the only
allowed operations are read-only: `git status`, `git log`, `git diff`,
`git show`, file reads. **Forbidden in the main checkout's working tree**:
edits, `git reset --hard`, `git checkout .`, `git stash`, `git restore .`,
`cargo build`/`cargo test` (write to `target/`), `sed`/`awk`/`patch`, or
any command that mutates the working tree, the index, or `target/`.

🔴 **All write-side work happens in a dedicated git worktree branched off
`origin/main`.** Provision one before starting any edit, build, or test:

```bash
git fetch origin main
git worktree add -b <feature-or-fix-branch> \
                  .claude/worktrees/<dirname> origin/main
cd .claude/worktrees/<dirname>
# … edit, build, test, commit, push from here …
```

Each ticket, refactor, or experiment gets its own worktree. Worktrees are
disposable — delete them with `git worktree remove --force <path>` once the
PR has merged.

🟡 **If you absolutely must run a command from the main checkout** — for
example `cargo install --path crates/<name> --locked` after a merge —
stash first, operate, then restore:

```bash
git -C /path/to/main-checkout stash push -u \
    -m "claude: pre-op-safety $(date +%s)"
# … do the op …
git -C /path/to/main-checkout stash pop
```

Surface the stash name in your report if popping fails so the human can
restore manually.

🟡 **`cargo install` from a worktree, not the main checkout.** The preferred
pattern for installing a freshly-built binary onto your PATH is:

```bash
cargo install --path .claude/worktrees/<dirname>/crates/<name> --locked
```

Cargo writes atomically to a temp file and renames into `~/.cargo/bin/`,
which keeps the macOS kernel's cdhash cache consistent (see the
release-workflow note above). The main checkout never needs to be involved.

🟢 **Subagents inherit these rules.** Every `Agent`/`Task` dispatch prompt
**must**:
- name the exact worktree path the agent should operate from
- explicitly forbid leaving that worktree into the main checkout
- forbid `git reset --hard`, `git checkout .`, and `git stash` against the
  main checkout
- forbid touching files outside the assigned worktree

The pattern of instructing an agent to "operate from the main checkout" is
banned. QA agents get their own worktree
(`.claude/worktrees/qa-<ticket-or-pass>`) just like engineering agents.

🟢 **Worktree cleanup is safe.** `git worktree remove --force <path>` deletes
the worktree directory but never the main checkout. After a squash-merge the
local feature branch will appear "unmerged" to git because the squashed
commit on `main` has a different hash — use `git branch -D <branch>` and
`git push origin --delete <branch>` to clean up. These operations touch only
refs, never working trees.

## Per-Crate Reference

Detailed implementation information for each crate lives in its own documentation:

- **trusty-common** — see `crates/trusty-common/README.md` and `docs/trusty-common/`
- **trusty-embedderd** — see `crates/trusty-embedderd/README.md` and `docs/trusty-embedderd/` (fastembed sidecar daemon)
- **trusty-bm25-daemon** — see `crates/trusty-bm25-daemon/README.md` and `docs/trusty-bm25-daemon/` (BM25 index sidecar)
- **trusty-memory** — see `crates/trusty-memory/README.md` and `docs/trusty-memory/` (licensed MIT, not Elastic-2.0; storage engine lives in `trusty-common`'s `memory-core` feature)
- **trusty-search** — see `crates/trusty-search/README.md` and **`docs/trusty-search/`** (primary worked example with regression testing, research, sessions)
- **trusty-analyze** — see `crates/trusty-analyze/README.md` and `docs/trusty-analyze/` (licensed MIT, not Elastic-2.0)
- **trusty-mpm** — see `crates/trusty-mpm/README.md` and `docs/trusty-mpm/` (unified platform: CLI binaries `tm`/`trusty-mpm`, daemon, MCP server, TUI, Telegram)
- **trusty-mpm-gui** — see `crates/trusty-mpm-gui/README.md` (Tauri desktop GUI, publish=false)
- **trusty-agents** — see `crates/trusty-agents/README.md` and `docs/trusty-agents/` (agent orchestration platform, bin: `tagent`)
- **trusty-agents-common** — see `crates/trusty-agents-common/README.md` (common API types for trusty-agents, publish=false)
- **trusty-agents-local** — see `crates/trusty-agents-local/README.md` (local execution engine for trusty-agents, publish=false)
- **trusty-git-analytics** — see `crates/trusty-git-analytics/README.md` and `docs/trusty-git-analytics/`
- **trusty-controller** — see `crates/trusty-controller/README.md` and `docs/trusty-controller/` (Phase 0 scaffold, bin: `tctl`; publish=false until Phase 1+; RFC #920)

For license details, check each crate's `Cargo.toml`: most are **Elastic License 2.0**, but `trusty-memory`, `trusty-analyze`, and a few others are **MIT**.

## Abbreviations & Aliases

When the user (or any agent) refers to a crate by abbreviation, resolve it using this table before taking any action.

| Abbreviation | Full crate name | Cargo package flag | Directory |
|---|---|---|---|
| `tga` | trusty-git-analytics | `-p tga` | `crates/trusty-git-analytics/` |
| `tm` | trusty-memory | `-p trusty-memory` | `crates/trusty-memory/` |
| `ts` | trusty-search | `-p trusty-search` | `crates/trusty-search/` |
| `tc` | trusty-common | `-p trusty-common` | `crates/trusty-common/` |
| `ta` | trusty-analyze | `-p trusty-analyze` | `crates/trusty-analyze/` |
| `mpm` | trusty-mpm | `-p trusty-mpm` | `crates/trusty-mpm/` |
| `tagent` or `t-agents` | trusty-agents | `-p trusty-agents` | `crates/trusty-agents/` (bin: `tagent`) |
| `t-agents-common` | trusty-agents-common | `-p trusty-agents-common` | `crates/trusty-agents-common/` |
| `t-agents-local` | trusty-agents-local | `-p trusty-agents-local` | `crates/trusty-agents-local/` |
| `tcode` | trusty-code | `-p trusty-code` | `crates/trusty-code/` |
| `tctl` | trusty-controller | `-p trusty-controller` | `crates/trusty-controller/` |

These abbreviations apply everywhere: ticket descriptions, build commands, references in conversation. Always expand before running `cargo` commands.

> **Auto-resolution:** When connected to trusty-memory MCP, call `get_prompt_context()` at the start of each turn to load current aliases and conventions. Pass a `query` string to filter to relevant facts only.

## Development Environment

### Required Tools

- **Rust**: `rustup` with the toolchain pinned to MSRV `1.91` or later.
  Install: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **Node / pnpm**: only needed if working on the Svelte UIs embedded in
  `trusty-search` or `trusty-memory`. Install pnpm via `npm i -g pnpm`.
- **Git**: standard; the workspace uses git tags for per-crate releases.

### Environment Variables

| Variable | Required by | Purpose |
|---|---|---|
| `OPENROUTER_API_KEY` | `trusty-search` `/chat`, `trusty-common` chat helpers, `trusty-analyze` deep pass (OpenRouter path) | LLM chat via OpenRouter. Pass as argument to library helpers; never read from env inside library crates. Required for `POST /analyze/deep` unless a `bedrock/<model-id>` model is selected. |
| `TRUSTY_LLM_MODEL` | `trusty-analyze` deep pass | LLM model id for the deep-analysis narrative pass. Default: `openai/gpt-4o-mini` (OpenRouter). Set to `bedrock/<bedrock-model-id>` (e.g. `bedrock/us.anthropic.claude-sonnet-4-6`) to route through AWS Bedrock instead of OpenRouter. The `bedrock/` prefix selects the Bedrock provider; anything else routes to OpenRouter. Claude Sonnet 4.6 uses the short form without date stamp or `-v1:0` suffix. |
| `TRUSTY_AWS_REGION` | `trusty-analyze` (Bedrock deep pass) | AWS region for Bedrock `Converse` calls. Takes priority over `AWS_REGION`. Default: `us-east-1`. |
| `AWS_REGION` | `trusty-analyze` (Bedrock deep pass) | Fallback AWS region for Bedrock calls. Overridden by `TRUSTY_AWS_REGION`. |
| `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN` | `trusty-analyze` (Bedrock deep pass) | Standard AWS credentials for Bedrock access. The full AWS credential chain (env vars, `~/.aws/credentials` profiles, IAM roles, SSO) is supported. No API key is needed when using a `bedrock/` model. |
| `RUST_LOG` | all daemons | Tracing filter, e.g. `RUST_LOG=debug` or `RUST_LOG=trusty_search=debug,warn`. |
| `TRUSTY_MEMORY_LIMIT_MB` | `trusty-search` | Soft RSS ceiling for indexing pipeline. Auto-tuned from system RAM; override only when needed. |
| `TRUSTY_MAX_CHUNKS` | `trusty-search` | Hard cap on chunks per index. Auto-tuned; rarely set manually. |
| `TRUSTY_MAX_BATCH_SIZE` | `trusty-search` | ONNX embedding batch size. Auto-tuned; set if OOM during reindex. |
| `TRUSTY_EMBEDDING_CACHE` | `trusty-search` | LRU embedding cache capacity (entries). |
| `TRUSTY_COREML_TRIPWIRE_MB` | `trusty-search` (Apple Silicon) | RSS-delta ceiling per CoreML batch (default 4 GB). If exceeded, batch size is halved automatically. Override for hosts with different memory pressure characteristics. |
| `TRUSTY_GPU_MEM_LIMIT_BYTES` | `trusty-search` / `trusty-embedderd` (CUDA EP, issue #600) | Exact CUDA `gpu_mem_limit` in bytes, applied alongside `arena_extend_strategy=kSameAsRequested` to stop ORT's BFCArena over-reserving VRAM and OOMing a 16 GB Tesla T4. Default 12 GiB (`12884901888`). Takes precedence over `TRUSTY_GPU_MEM_LIMIT_MB`; a malformed or `0` value is ignored. Removes the need for the old `TRUSTY_MAX_BATCH_SIZE=32` workaround. |
| `TRUSTY_GPU_MEM_LIMIT_MB` | `trusty-search` / `trusty-embedderd` (CUDA EP, issue #600) | CUDA `gpu_mem_limit` in megabytes (scaled by 1024²). Used only when `TRUSTY_GPU_MEM_LIMIT_BYTES` is unset/invalid. E.g. `6144` for an 8 GB card. |
| `ORT_DYLIB_PATH` | `trusty-search` (CUDA, glibc < 2.38); `trusty-analyze` (`load-dynamic`, `cuda` features) | Path to `libonnxruntime.so` on hosts with glibc < 2.38 and CUDA builds. For trusty-analyze, install with `--no-default-features --features http-server,load-dynamic` and set this var to the system libonnxruntime path. |
| `SKIP_UI_BUILD` | `trusty-search` `build.rs` | Set to `1` to skip the Svelte UI build step (CI publish flows). |
| `TRUSTY_NO_KG` | `trusty-search` daemon | Machine-wide default for `skip_kg`. When set to `1`, `true`, or `yes`, every new index created via `POST /indexes` (or `trusty-search index`) has `skip_kg=true` applied automatically unless the caller explicitly sets `skip_kg: false`. Useful for CI machines or resource-constrained hosts where KG is never needed. |

### Recommended IDE Setup

**VS Code / Cursor**:
- Install `rust-analyzer` extension.
- Install `Even Better TOML` for `Cargo.toml` editing.
- Workspace-level `rust-analyzer` picks up the root `Cargo.toml` automatically;
  no per-crate `.vscode/settings.json` needed.
- Recommended settings in `.vscode/settings.json`:
  ```json
  {
    "rust-analyzer.cargo.features": "all",
    "rust-analyzer.checkOnSave.command": "clippy"
  }
  ```

**RustRover**: open the repo root; it detects the workspace automatically.

### Running Individual MCP Servers Locally

Each MCP server reads from stdin / writes to stdout (JSON-RPC 2.0 framing).
All daemons log to **stderr** — never stdout.

```bash
# trusty-search daemon (HTTP + MCP stdio)
RUST_LOG=info cargo run -p trusty-search -- start
# Query via CLI
cargo run -p trusty-search -- query "fn authenticate" --index <id>
# MCP stdio mode (used by Claude Code via .mcp.json)
cargo run -p trusty-search -- serve

# MPM daemon
RUST_LOG=info cargo run -p trusty-mpm --bin trusty-mpmd

# MPM CLI (tm / trusty-mpm)
cargo run -p trusty-mpm -- --help

# trusty-memory (MCP server + embedded Svelte UI)
RUST_LOG=info cargo run -p trusty-memory

# Report the daemon's listening port (stdout is clean — safe for shell substitution):
trusty-search port                                   # bare port: 7879
trusty-search port --addr                            # host:port: 127.0.0.1:7879
trusty-search port --json                            # {"addr":"127.0.0.1","port":7879}
# Shell substitution idiom — queries the daemon without guessing the port:
curl http://127.0.0.1:$(trusty-search port)/health

trusty-memory port                                   # bare port: 7070
trusty-memory port --addr                            # host:port: 127.0.0.1:7070
trusty-memory port --json                            # {"addr":"127.0.0.1","port":7070}
curl http://127.0.0.1:$(trusty-memory port)/health

# Fire-and-forget memory note from any agent (no MCP tool needed):
# Sub-agents spawned via Claude Code's Agent tool do not inherit MCP
# connections, so they cannot call `mcp__trusty-memory__memory_remember`
# directly. The `note` subcommand POSTs to the daemon's HTTP endpoint
# (`POST /api/v1/remember`) and returns immediately — the dispatch runs
# on a detached `tokio::spawn`. Failures degrade to stderr + zero exit.
trusty-memory note "key fact here" --palace my-project
trusty-memory note "another fact" --palace my-project --tag style --tag preferences

# Build a specific binary in release mode
cargo build --release -p trusty-search
./target/release/trusty-search start
```

To wire a locally-built binary into Claude Code, update your project's
`.mcp.json` or `~/.claude/mcp.json` to point `command` at the absolute path
of the built binary (e.g. `target/release/trusty-search`).

## Common Pitfalls

🔴 **Using `unwrap()` in library crates** — the compiler does not stop you, but
it violates the project's hard rule. Use `?` with `thiserror` error types in
libraries. `expect()` is allowed only for invariants that genuinely cannot
occur at runtime (not for "I think this will always be Some").

🔴 **Logging to stdout in a daemon or MCP server** — MCP JSON-RPC framing uses
stdout as the transport channel. A stray `println!` corrupts the protocol.
Always use `tracing::info!` / `tracing::debug!` etc. (which write to stderr).

🔴 **Adding `axum` as an unconditional dependency in a library crate** — put it
behind the `axum-server` feature flag, matching the pattern in `trusty-common`.
Otherwise every library consumer pulls in the full axum + tower stack.

🟡 **Editing a shared crate without propagating changes** — modifying
`trusty-common` (or its consolidated `symgraph` / `embedder` / `mcp` modules),
`trusty-embedderd`, or `trusty-bm25-daemon` can silently break dependents. Always run `cargo check` (workspace-wide) and
`cargo test -p <consumer>` for every crate that imports the edited library.

🟡 **Forgetting the Why/What/Test doc pattern on new public items** — clippy
does not enforce this. Review public APIs manually before committing.

🟡 **Building the Svelte UI manually before `cargo build`** — `trusty-search`
uses `build.rs` to invoke pnpm if `ui-dist/` is stale. If pnpm is not
installed, the build script fails loudly. Install pnpm or set
`SKIP_UI_BUILD=1` if you are not changing the UI.

🟡 **`[patch.crates-io]` only works at the workspace root** — do not add
`[patch]` tables inside individual crate `Cargo.toml` files; Cargo ignores
them. All patches must live in the root `Cargo.toml`.

🔴 **Growing a file past its SLOC cap instead of splitting** — the compiler does
not stop you, but continued feature additions make the module harder to review,
reason about, and test. Split proactively. The applicable cap is **500 SLOC for
production files** and **1500 SLOC for test/benchmark files** (see the Key
Conventions section for the exact classification rules). SLOC counts code lines
only: blank lines, `//` comments, `///` doc comments, `//!` inner-doc comments,
and `/* ... */` block comments (including multi-line spans) are all excluded.
The trusty-agents `ctrl/`, `runtime/`, and `workflow/engine/` modules (#170,
#171, #172) were the canonical examples of files that grew past the prod cap;
all three have since been split into focused submodules and now serve as the
worked examples of a clean split.

🟢 **MSRV drift** — the workspace pins `rust-version = "1.91"`. Running
`rustup update` and picking up a new nightly may introduce syntax that
compiles locally but fails on CI. Prefer stable channel toolchains.

🟢 **Edition mismatch** — `trusty-mpm`, `trusty-mpm-gui`, `trusty-agents`, `trusty-agents-common`, and `trusty-agents-local` use edition 2024;
all other crates use edition 2021. Let-chains (`if let … && let …`) only
work in edition 2024. Do not copy let-chain patterns into edition-2021 crates.

## Former Repos Reference

These repos were merged into this monorepo. Use this table when reading old
PRs, issues, or commit messages that reference the former repo names.

| Former repo | Now lives in |
|---|---|
| `bobmatnyc/trusty-common` | `crates/trusty-common` + 8 library crates |
| `bobmatnyc/trusty-search` | `crates/trusty-search` |
| `bobmatnyc/trusty-memory` | `crates/trusty-common` (`memory-core` feature — storage engine) + `crates/trusty-memory` (MCP frontend) |
| `bobmatnyc/trusty-analyze` | `crates/trusty-analyze` |
| `bobmatnyc/trusty-git-analytics` | `crates/trusty-git-analytics` |
| `bobmatnyc/trusty-mpm` | `crates/trusty-mpm/` (unified crate) + `crates/trusty-mpm-gui/` |
| `bobmatnyc/open-mpm` | `crates/trusty-agents` (renamed from `open-mpm` in #831) |
