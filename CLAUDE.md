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

> **Full release workflow and macOS critical notes:** see [docs/reference/release-workflow.md](docs/reference/release-workflow.md).

### Quick Release Steps

1. Bump the crate version in `crates/<name>/Cargo.toml`.
2. Update any dependent crates that pin that version.
3. Run `cargo test -p <name>` and `cargo clippy --workspace -- -D warnings`.
4. Commit the version bump.
5. Create the tag: `git tag <crate-name>-v<version>`.
6. Push the tag: `git push origin <crate-name>-v<version>`.
7. Publish: `cargo publish -p <crate-name>` (or `SKIP_UI_BUILD=1 cargo publish` for UI-embedding crates).
8. Build and install: `cargo install --path crates/<dir> --locked`.

🔴 **CRITICAL macOS note:** Never use `cp` to install release binaries on macOS — always use `cargo install`. See release workflow reference for the detailed explanation.

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

**CRITICAL RULES FOR CONCURRENT SESSIONS:**

🔴 **The main checkout is inspection-only.** From the repo root
(`/path/to/trusty-tools/`), the only allowed operations are read-only: `git status`,
`git log`, `git diff`, `git show`, file reads. **FORBIDDEN**: edits, `git reset --hard`,
`git checkout .`, `git stash`, `git restore .`, `cargo build`/`cargo test`,
`sed`/`awk`/`patch`, or any command that mutates the working tree, index, or `target/`.

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
**must** name the exact worktree path the agent should operate from and forbid
leaving that worktree into the main checkout, `git reset --hard`, `git checkout .`,
and `git stash` against the main checkout, and touching files outside the assigned worktree.

The pattern of instructing an agent to "operate from the main checkout" is banned.
QA agents get their own worktree (`.claude/worktrees/qa-<ticket-or-pass>`) just
like engineering agents.

🟢 **Worktree cleanup is safe.** `git worktree remove --force <path>` deletes
the worktree directory but never the main checkout. Use `git branch -D <branch>`
and `git push origin --delete <branch>` to clean up refs after a squash-merge.

> **Extended discipline rationale and cleanup details:** see [docs/reference/worktree-discipline.md](docs/reference/worktree-discipline.md).

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

> **Full environment-variable reference:** see [docs/reference/environment-variables.md](docs/reference/environment-variables.md).

Key variables:
- `OPENROUTER_API_KEY` — LLM chat via OpenRouter
- `TRUSTY_LLM_MODEL` — LLM model for deep-analysis pass (default: `openai/gpt-4o-mini`)
- `RUST_LOG` — Tracing filter (e.g., `RUST_LOG=debug`)
- `SKIP_UI_BUILD` — Set to `1` to skip Svelte UI build in `build.rs`
- `TRUSTY_NO_KG` — Set to `1` to skip knowledge-graph construction by default

### IDE Setup

> **Full IDE setup reference:** see [docs/reference/ide-setup.md](docs/reference/ide-setup.md).

Quick: VS Code needs `rust-analyzer` + `Even Better TOML` extensions; RustRover auto-detects the workspace.

### Running Individual MCP Servers Locally

> **Detailed MCP server examples and wiring:** see [docs/reference/running-mcp-servers.md](docs/reference/running-mcp-servers.md).

Quick: `RUST_LOG=info cargo run -p trusty-search -- start` (daemon), `cargo run -p trusty-search -- serve` (MCP stdio mode).

## Common Pitfalls — Quick Checklist

For extended explanations, see [docs/reference/common-pitfalls.md](docs/reference/common-pitfalls.md).

- **Library error handling:** use `thiserror`, not `unwrap()` in libraries
- **Daemon stdout:** never log to stdout in daemons or MCP servers
- **Axum in libraries:** gate behind `axum-server` feature flag
- **Shared crate changes:** always run `cargo check` + tests for all dependents
- **SLOC cap:** respect 500/1500 SLOC limits (prod/test); use `bash scripts/check_line_cap.sh`
- **UI build:** install pnpm or set `SKIP_UI_BUILD=1` before `cargo build`
- **Patch tables:** put all `[patch.crates-io]` in root `Cargo.toml` only
- **MSRV drift:** prefer stable channel toolchains; don't break `rust-version = "1.91"`
- **Edition mismatch:** 2024 crates (mpm, agents, mpm-gui, agents-common, agents-local) may use let-chains; 2021 crates cannot

## Reference Documentation

Full-length reference materials for less-frequent lookups:

- **Code structure & crate map:** [docs/reference/crate-map.md](docs/reference/crate-map.md)
- **Documentation layout conventions:** [docs/reference/documentation-layout.md](docs/reference/documentation-layout.md)
- **Former repos (monorepo history):** [docs/reference/former-repos.md](docs/reference/former-repos.md)
- **Release workflow (with macOS signing details):** [docs/reference/release-workflow.md](docs/reference/release-workflow.md)
- **Worktree discipline (extended rationale):** [docs/reference/worktree-discipline.md](docs/reference/worktree-discipline.md)
- **Common pitfalls (detailed explanations):** [docs/reference/common-pitfalls.md](docs/reference/common-pitfalls.md)
- **Environment variables (full table):** [docs/reference/environment-variables.md](docs/reference/environment-variables.md)
- **IDE setup (detailed):** [docs/reference/ide-setup.md](docs/reference/ide-setup.md)
- **Running MCP servers (examples & wiring):** [docs/reference/running-mcp-servers.md](docs/reference/running-mcp-servers.md)
