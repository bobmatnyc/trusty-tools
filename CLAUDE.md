# trusty-tools — Claude Code Instructions

Unified Rust workspace consolidating the entire trusty-* AI tooling ecosystem.
16 crates — shared libraries, daemon/MCP servers, the MPM platform, and an
orchestrator — all co-located under one Cargo workspace.

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

The workspace root contains `crates/` (16 members, one per subdirectory). Each crate's `README.md` describes its purpose, layout, and dependencies. For a complete map, see [docs/reference/crate-map.md](docs/reference/crate-map.md).

Note: formerly separate `trusty-symgraph`, `trusty-rpc`, `trusty-tickets`, `trusty-mcp-core`, `trusty-embedder`, `trusty-memory-core`, and `trusty-monitor-tui` crates were consolidated into `trusty-common` behind feature flags (`symgraph`, `rpc`, `tickets`, `mcp`, `embedder`, `memory-core`, `monitor-tui`).

## Documentation Layout

Documentation is organized by **published crate**, not by topic. Each crate gets a directory under `docs/` with three standard subdirectories: `regression-testing/` (versioned snapshots), `research/` (investigations), and `sessions/` (engineering narratives).

See **`docs/trusty-search/`** as the authoritative worked example, and read [docs/reference/documentation-layout.md](docs/reference/documentation-layout.md) for the full convention and cross-release tracking details.

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

🔴 **500-line file size hard cap (MECHANICALLY ENFORCED)** — no source file
(`.rs`) should exceed 500 lines. As of issue #610 this is no longer advice: it
is gated by `scripts/check_line_cap.sh`, wired into CI
(`.github/workflows/line-cap.yml`) and the local pre-commit hook (`line-cap`).
A new tracked `.rs` file over 500 lines **cannot merge**. Files approaching this
limit are a signal to split into focused submodules before the next feature
lands on them. When splitting, prefer: one public module per logical concept, a
thin `mod.rs` that re-exports, and sibling files with clear single
responsibilities.

**The ratchet (allowlist that can only shrink):** the 175 files that already
exceeded the cap when the gate landed are grandfathered in
`.line-cap-allowlist.tsv` (one `relative/path<TAB>budget` line each, where
`budget` is that file's frozen max line count). The gate enforces:

- a file ≤ 500 lines and **not** allowlisted → OK;
- a file > 500 lines and **not** allowlisted → **FAIL** (new oversized file — split it);
- an allowlisted file whose current count **exceeds its budget** → **FAIL** (it grew — split it);
- an allowlisted file now **≤ 500 lines** → **FAIL** (it dropped under cap — remove its allowlist entry; this is the ratchet-down forcing function);
- an allowlisted file with `500 < lines ≤ budget` → OK (grandfathered, not growing).

So allowlisted files may only shrink, and no new oversized file may be added.
As the #607 sweep and per-crate refactors land, the allowlist ratchets down
toward empty.

**Run it locally:** `bash scripts/check_line_cap.sh` (exit 0 = clean). After you
intentionally split a file (or a file otherwise drops below its budget), refresh
the frozen budgets with `scripts/check_line_cap.sh --update` — this only *lowers*
budgets or *removes* entries that fell ≤ 500; it **refuses** to add a new
oversized file or raise a budget unless you pass `--seed` (initial bootstrap) or
`--force-add` (rare, intentional bump). Commit the regenerated
`.line-cap-allowlist.tsv` alongside your split.

Past violations (refactor tickets #170/#171/#172 are CLOSED and the splits have
landed — all three former monoliths are now under the 500-line cap):
- `crates/trusty-agents/src/ctrl/mod.rs` — RESOLVED (#170). Split into focused
  submodules under `crates/trusty-agents/src/ctrl/` (`state`, `config`, `repl`,
  `handlers`, `pm_task`, …); `mod.rs` is now a ~50-line re-export facade.
- `crates/trusty-agents/src/runtime/` — RESOLVED (#171). The original `runtime.rs`
  was split into a `runtime/` module; every submodule is now under the cap.
- `crates/trusty-agents/src/workflow/engine/` — RESOLVED (#172). The original
  `engine.rs` was split into an `engine/` module; every submodule is now under
  the cap (largest is `engine/executor/run.rs` at ~485 lines).

The largest remaining files in `trusty-agents` (none tied to an open ticket) are
`tools/memory/tests.rs` (~605) and `tm/manager.rs` (~570) — file a fresh refactor
ticket before growing those further.

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

Each crate is tagged independently: `<crate-name>-v<version>` (e.g. `trusty-mcp-core-v0.2.0`). Version comes from the crate's `Cargo.toml`.

**Release workflow** (when publishing, bump only changed crates — see #343):

1. Bump version in `crates/<name>/Cargo.toml`.
2. Update dependent crates that pin that version.
3. Run `cargo test -p <name>` and `cargo clippy --workspace -- -D warnings`.
4. Commit the version bump.
5. Create tag: `git tag <crate-name>-v<version>`.
6. Push tag: `git push origin <crate-name>-v<version>`.
7. Publish: `cargo publish -p <crate-name>`.
   - **UI-embedding crates** (trusty-search, trusty-memory, trusty-analyze): `SKIP_UI_BUILD=1 cargo publish -p <crate-name>` (ui-dist/ is prebuilt; build.rs fails without pnpm in verification tarball).
8. Build release binary: `cargo build --release -p <crate-name>`.
9. Install locally: `cargo install --path crates/<dir> --locked`.

🔴 **Never `cp target/release/<binary> ~/.cargo/bin/<binary>` on macOS.** `cargo build` signs binaries; the kernel's code-signing cache is keyed by `cdhash`. Copying over an existing binary leaves a stale cached identity, causing `EXC_CRASH / CODESIGNING` kills. Use `cargo install` (atomic rename) or follow manual copy with `codesign --force --sign - ~/.cargo/bin/<binary>`.

🔴 **After every `cargo install trusty-search` on macOS, re-grant Full Disk Access** (issue #873): each install writes a new file with new cdhash, invalidating the TCC grant. Symptom: `trusty-search status` shows `indexes:2` and warm-boot logs show `tcc=57`. Steps: Settings → Privacy → Full Disk Access → remove/re-add `~/.cargo/bin/trusty-search` → restart daemon. The daemon auto-detects degraded warm-boot and logs the hint.

**Connection-safe daemon restart (issue #534):** Daemons (trusty-memory, trusty-search, trusty-analyze) support graceful shutdown (SIGTERM drains in-flight requests). Use `launchctl bootout` (SIGTERM), not `kickstart -k` (SIGKILL). Re-grant FDA after install.

For full details, see [docs/reference/release-workflow.md](docs/reference/release-workflow.md).

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

Multiple sessions share this repo. To prevent interference, follow this mandatory pattern:

🔴 **Main checkout is read-only:** `git status`, `git log`, `git diff`, `git show`, file reads only. Forbidden: edits, builds, `git reset --hard`, `cargo build`, `sed`/`awk`.

🔴 **All writes happen in a dedicated worktree:**
```bash
git fetch origin main
git worktree add -b <feature-or-fix-branch> \
                  .claude/worktrees/<dirname> origin/main
cd .claude/worktrees/<dirname>
# … edit, build, test, commit, push from here …
```

🟡 **Emergency main-checkout operations:** stash first, operate, restore (with stash name in report if pop fails).

🟡 **Preferred `cargo install`:** from a worktree (keeps cdhash cache consistent on macOS).

🟢 **Subagents:** must name exact worktree path, forbid main-checkout access, forbid touching files outside worktree.

🟢 **Cleanup:** `git worktree remove --force <path>` is safe; it never touches the main checkout.

For full rationale and edge cases, see [docs/reference/worktree-discipline.md](docs/reference/worktree-discipline.md).

## Per-Crate Reference

See [docs/reference/crate-map.md](docs/reference/crate-map.md) for a full map of each crate's location, licensing, and documentation links.

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

Full environment-variable reference: see [docs/reference/environment-variables.md](docs/reference/environment-variables.md).

### Recommended IDE Setup

See [docs/reference/ide-setup.md](docs/reference/ide-setup.md) for VS Code/Cursor and RustRover configuration.

### Running Individual MCP Servers Locally

See [docs/reference/running-mcp-servers.md](docs/reference/running-mcp-servers.md) for commands, port discovery, and wiring binaries into Claude Code.

## Common Pitfalls

Quick checklist of common mistakes. For full explanations and rationale, see [docs/reference/common-pitfalls.md](docs/reference/common-pitfalls.md).

- 🔴 `unwrap()` in libraries — use `?` + `thiserror`; `expect()` only for invariants
- 🔴 Logging to stdout in daemons/MCP — corrupts JSON-RPC; use `tracing` (stderr)
- 🔴 `axum` unconditional in libraries — gate behind `axum-server` feature flag
- 🟡 Shared crate changes without propagating — run `cargo check` + `cargo test -p <consumer>` for all dependents
- 🟡 Missing Why/What/Test docs on public items — clippy won't catch; manual review required
- 🟡 Manual Svelte UI builds — let `build.rs` handle it; use `SKIP_UI_BUILD=1` if needed
- 🟡 `[patch.crates-io]` in crate `Cargo.toml` — Cargo ignores per-crate patches; use root only
- 🔴 Files exceeding 500 lines — split proactively (see line-cap section in Key Conventions)
- 🟢 MSRV drift — pin to stable; nightly syntax fails on CI's locked MSRV 1.91
- 🟢 Edition mismatch — let-chains require edition 2024; don't copy into 2021 crates

## Former Repos Reference

See [docs/reference/former-repos.md](docs/reference/former-repos.md) to map old repo names to their current locations in the monorepo.
