# trusty-tools — Claude Code Instructions

Unified Rust workspace consolidating the entire trusty-* AI tooling ecosystem.
22 crates — shared libraries, daemon/MCP servers, the MPM platform, and an
orchestrator — all co-located under one Cargo workspace.

## Project Overview

This is a **Rust workspace** (Cargo workspace, resolver v2, glob members
`crates/*`) under the Elastic License 2.0 (per-crate; a few crates are MIT —
see each `Cargo.toml`). The workspace version field and `rust-version` are
inherited by the `trusty-mpm-*` family; other crates set their own version.

**MSRV**: `1.88` — required for stabilised let-chains used by edition-2024 crates.

## Role & Scope

`trusty-tools` is the **single source of truth** for all trusty-* AI tooling.
It replaces seven formerly separate repos and eliminates the `[patch.crates-io]`
dance for cross-crate development. The authoritative crate list is the
`[workspace.members]` glob in the root `Cargo.toml`; every subdirectory under
`crates/` with a `Cargo.toml` is a member.

Key consumers of the shared libraries:
- **open-mpm** — MPM orchestration platform (lives in `crates/open-mpm`)
- **trusty-search** — hybrid code search daemon + MCP server
- **trusty-memory-core / trusty-memory** — memory palace storage + MCP frontend
- **trusty-analyze** — code analysis daemon (complexity, smells, quality metrics)

Work touching a shared crate (e.g. `trusty-common`, `trusty-mcp-core`) may
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
cargo run -p trusty-mpm-cli -- --help

# Build only the binary, not the whole workspace
cargo build --release -p trusty-mpm-cli

# Lint a single crate
cargo clippy -p trusty-search -- -D warnings

# Test a single crate with ignored tests
cargo test -p trusty-embedder -- --include-ignored

# Run trusty-search performance regression suite (requires daemon + indexed trusty-tools)
cargo test -p trusty-search --test baseline_trusty_tools -- --include-ignored --nocapture
```

### Important: Crate Names vs. Directory Names

**Crate names** match the `name` field in each crate's `Cargo.toml`, not necessarily the directory name.
Most match (e.g. `crates/trusty-search/` → `-p trusty-search`) but note these exceptions:

- `crates/trusty-git-analytics/` → `-p tga` (short name)
- `crates/open-mpm/` → `-p open-mpm`

Always verify the `name` field in the crate's `Cargo.toml` if you get a "package not found" error.

## Code Structure

```
trusty-tools/               # workspace root
├── Cargo.toml              # workspace manifest — glob members = ["crates/*"]
├── Cargo.lock
├── crates/
│   ├── trusty-common/      # shared utilities, tracing, OpenRouter chat
│   ├── trusty-embedder/    # fastembed wrapper (AllMiniLML6V2Q, 384-dim)
│   ├── trusty-mcp-core/    # MCP primitives and JSON-RPC types
│   ├── trusty-symgraph/    # symbol graph engine (tree-sitter parser)
│   ├── trusty-rpc/         # RPC helpers and service descriptors
│   ├── trusty-tickets/     # GitHub Issues ticketing integration
│   ├── trusty-gworkspace/  # Google Workspace client (Calendar, Tasks, Drive)
│   ├── trusty-cto-db/      # SQLite CTO database (rusqlite-backed)
│   ├── tc-services/        # service-layer adapters: CTO DB, Granola, GWorkspace
│   ├── trusty-search/      # hybrid BM25 + vector + KG search daemon + MCP server
│   ├── trusty-memory-core/ # re-export shim — absorbed into trusty-common's memory-core feature
│   ├── trusty-memory/      # MCP server frontend for memory (includes Svelte UI)
│   ├── trusty-analyze/     # code analysis daemon + MCP server
│   ├── trusty-mpm-core/    # MPM core domain types and traits
│   ├── trusty-mpm-mcp/     # MCP server for MPM
│   ├── trusty-mpm-daemon/  # MPM background daemon service
│   ├── trusty-mpm-client/  # MPM API client library
│   ├── trusty-mpm-cli/     # CLI binary (trusty-mpm / tm)
│   ├── trusty-mpm-tui/     # MPM terminal UI
│   ├── trusty-mpm-telegram/ # MPM Telegram bot integration
│   ├── trusty-mpm-gui/     # MPM desktop GUI (Tauri)
│   ├── trusty-git-analytics/ # developer productivity analytics (tga)
│   └── open-mpm/           # MPM orchestration platform
└── .gitignore
```

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
├── open-mpm/
├── trusty-analyze/
└── trusty-git-analytics/
```

**Purpose of each subdir**:
- **`regression-testing/`** — Performance snapshots tied to releases. One `.md`
  file per measured release named `v{VERSION}-{YYYY-MM-DD}.md`; alternate-corpus
  baselines (e.g., synthetic, open-mpm) live alongside; `current.md` is a
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

🔴 **`thiserror` for libraries, `anyhow` for binaries** — library crates
(`trusty-common`, `trusty-mcp-core`, etc.) define structured error enums with
`#[derive(thiserror::Error)]`. Binary and daemon crates use `anyhow::Result`
throughout.

🔴 **Feature flags** — `trusty-common` gates `axum` and `tower-http` behind the
`axum-server` feature flag. Do not add axum as an unconditional dependency in
any library crate. Enable it explicitly in crates that serve HTTP.

🟡 **Rust editions** — `edition = "2024"` for `trusty-mpm-*` and `open-mpm`
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
8. Build the release binary (if not already fresh): `cargo build --release -p <crate-name>`.
9. Install the binary locally with `cargo install --path crates/<dir> --locked`
   (for crates with binaries, e.g. trusty-search, trusty-mpm-cli). This ensures the
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

The `trusty-mpm-*` family shares a single workspace version (declared under
`[workspace.package]`) and is bumped together.

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
- **trusty-embedder** — see `crates/trusty-embedder/README.md`
- **trusty-memory / trusty-memory-core** — see `crates/trusty-memory/README.md` and `docs/trusty-memory/` (licensed MIT, not Elastic-2.0)
- **trusty-search** — see `crates/trusty-search/README.md` and **`docs/trusty-search/`** (primary worked example with regression testing, research, sessions)
- **trusty-analyze** — see `crates/trusty-analyze/README.md` and `docs/trusty-analyze/` (licensed MIT, not Elastic-2.0)
- **trusty-mpm-cli, trusty-mpm-daemon, trusty-mpm-tui, trusty-mpm-gui, trusty-mpm-telegram** — see `crates/trusty-mpm-{variant}/README.md` and `docs/trusty-mpm/`
- **open-mpm** — see `crates/open-mpm/README.md` and `docs/open-mpm/`
- **trusty-git-analytics** — see `crates/trusty-git-analytics/README.md` and `docs/trusty-git-analytics/`

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
| `mpm` | trusty-mpm-cli | `-p trusty-mpm-cli` | `crates/trusty-mpm-cli/` |
| `open-mpm` | open-mpm | `-p open-mpm` | `crates/open-mpm/` |

These abbreviations apply everywhere: ticket descriptions, build commands, references in conversation. Always expand before running `cargo` commands.

> **Auto-resolution:** When connected to trusty-memory MCP, call `get_prompt_context()` at the start of each turn to load current aliases and conventions. Pass a `query` string to filter to relevant facts only.

## Development Environment

### Required Tools

- **Rust**: `rustup` with the toolchain pinned to MSRV `1.88` or later.
  Install: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **Node / pnpm**: only needed if working on the Svelte UIs embedded in
  `trusty-search` or `trusty-memory`. Install pnpm via `npm i -g pnpm`.
- **Git**: standard; the workspace uses git tags for per-crate releases.

### Environment Variables

| Variable | Required by | Purpose |
|---|---|---|
| `OPENROUTER_API_KEY` | `trusty-search` `/chat`, `trusty-common` chat helpers | LLM chat via OpenRouter. Pass as argument to library helpers; never read from env inside library crates. |
| `RUST_LOG` | all daemons | Tracing filter, e.g. `RUST_LOG=debug` or `RUST_LOG=trusty_search=debug,warn`. |
| `TRUSTY_MEMORY_LIMIT_MB` | `trusty-search` | Soft RSS ceiling for indexing pipeline. Auto-tuned from system RAM; override only when needed. |
| `TRUSTY_MAX_CHUNKS` | `trusty-search` | Hard cap on chunks per index. Auto-tuned; rarely set manually. |
| `TRUSTY_MAX_BATCH_SIZE` | `trusty-search` | ONNX embedding batch size. Auto-tuned; set if OOM during reindex. |
| `TRUSTY_EMBEDDING_CACHE` | `trusty-search` | LRU embedding cache capacity (entries). |
| `TRUSTY_COREML_TRIPWIRE_MB` | `trusty-search` (Apple Silicon) | RSS-delta ceiling per CoreML batch (default 4 GB). If exceeded, batch size is halved automatically. Override for hosts with different memory pressure characteristics. |
| `ORT_DYLIB_PATH` | `trusty-search` (CUDA, glibc < 2.38) | Path to `libonnxruntime.so` on hosts with glibc < 2.38 and CUDA builds. |
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
RUST_LOG=info cargo run -p trusty-mpm-daemon --bin trusty-mpmd

# MPM CLI (tm / trusty-mpm)
cargo run -p trusty-mpm-cli -- --help

# trusty-memory (MCP server + embedded Svelte UI)
RUST_LOG=info cargo run -p trusty-memory

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
`trusty-common`, `trusty-mcp-core`, `trusty-embedder`, or `trusty-symgraph`
can silently break dependents. Always run `cargo check` (workspace-wide) and
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

🟢 **MSRV drift** — the workspace pins `rust-version = "1.88"`. Running
`rustup update` and picking up a new nightly may introduce syntax that
compiles locally but fails on CI. Prefer stable channel toolchains.

🟢 **Edition mismatch** — `trusty-mpm-*` and `open-mpm` use edition 2024;
all other crates use edition 2021. Let-chains (`if let … && let …`) only
work in edition 2024. Do not copy let-chain patterns into edition-2021 crates.

## Former Repos Reference

These repos were merged into this monorepo. Use this table when reading old
PRs, issues, or commit messages that reference the former repo names.

| Former repo | Now lives in |
|---|---|
| `bobmatnyc/trusty-common` | `crates/trusty-common` + 8 library crates |
| `bobmatnyc/trusty-search` | `crates/trusty-search` |
| `bobmatnyc/trusty-memory` | `crates/trusty-common` (`memory-core` feature) + `crates/trusty-memory-core` (shim) + `crates/trusty-memory` (MCP frontend) |
| `bobmatnyc/trusty-analyze` | `crates/trusty-analyze` |
| `bobmatnyc/trusty-git-analytics` | `crates/trusty-git-analytics` |
| `bobmatnyc/trusty-mpm` | `crates/trusty-mpm-{core,mcp,daemon,client,cli,tui,telegram,gui}` |
| `bobmatnyc/open-mpm` | `crates/open-mpm` |
