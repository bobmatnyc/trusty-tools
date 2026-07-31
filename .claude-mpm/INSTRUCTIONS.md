## Session Startup Protocol

At the start of every session, before any other work, call the trusty-memory `get_prompt_context` MCP tool to load project aliases and conventions:

1. Call `get_prompt_context()` (no query param) via trusty-memory MCP
2. Apply all returned aliases immediately — any abbreviated crate name in user messages resolves via this table
3. If trusty-memory MCP is not available, skip silently and proceed — never block or warn the user

This call is mandatory and replaces manual context-setting. The result is used for the current session only and does not persist in the conversation history beyond the immediate turn.

## Ticket Source
GitHub Issues is the ticket source. Use `gh issue list`, `gh issue view`, `gh issue create` via version-control or ticketing agent.

## trusty-tools Monorepo Workflow

This is a unified Cargo workspace. Key rules:

### [patch.crates-io] rules
Internal-only crates (never published) resolve via workspace path deps — no `[patch.crates-io]` entry needed. Published sidecar lib crates (e.g. `trusty-embedderd`, `trusty-bm25-daemon`) DO need `[patch.crates-io]` entries in the workspace root `Cargo.toml` so local builds use the in-tree source. See the Single-Install Convention section below.

### Required Workflow Sequence

**Full delivery chain (spec → issue → worktree branch → PR → trusty-review → squash-merge → release):**

(prompt → ticket) OR (check tickets) → read ticket → **git fetch origin main** → **create worktree off origin/main** → implement → test → build → **commit & push → create PR** → **trusty-review gate** → **squash-merge to main** → version bump → publish → update consumers → verify CI

**Source of truth = `origin/main:HEAD`** — the main checkout is inspection-only. Always `git fetch origin main` and branch worktrees off `origin/main` because local main may be stale. Never commit directly to main; only through PR + trusty-review + squash-merge.

### Phase 0: Ticket
No work begins without a ticket reference.

### Phase 1: Read Ticket
Always read full ticket + comments before writing code.

### Phase 2: Implement
- Agent: rust-engineer (model: opus)
- No `unwrap()` in library code
- `thiserror` for crates, `anyhow` for binaries
- Why/What/Test doc pattern proportional to code surprisingness: full pattern for API entry points, design-heavy code, error contracts, safety/TCC behavior, cross-crate surfaces; one-line summaries for trivial items
- Changing a function/class because of this ticket: add an inline pointer at the change site (`// #1234: <one-line reason>`)

### Phase 3: Test
- `cargo test --workspace` — all green
- `cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo fmt --check` — clean

### Phase 4: Version Bump + Publish
- Semver: `feat` → minor, `fix`/`chore` → patch, `BREAKING` → major
- Tag format: `<crate-name>-v<version>`
- Publish: `cd crates/<name> && cargo publish`

### Commit Format
`feat|fix|chore|refactor|test|docs(<scope>): <description> (closes #N)`

### Cross-crate changes
When changing a shared library crate, update all crates in this workspace that depend on it. No separate repo coordination needed — everything is here.

### Former repos
The following repos are now READ-ONLY and point here: trusty-common, trusty-search, trusty-memory, trusty-analyze, trusty-git-analytics, trusty-mpm, open-mpm.

## Single-Install Convention

Each main crate's `cargo install` must produce **every binary required to run that crate**. Users invoke ONE `cargo install <main-crate>` command; sidecar daemons, helper binaries, and any other runtime executables are bundled automatically via `[[bin]]` targets in the main crate.

### How to bundle a sidecar binary into a main crate

1. The sidecar crate exposes a `[lib]` with a `pub async fn run() -> Result<()>` entry point (and keeps its own `src/main.rs` as a thin shim for standalone use if desired).
2. The main crate adds a `[[bin]]` target:
   ```toml
   [[bin]]
   name = "<sidecar-binary-name>"
   path = "src/bin/<sidecar-binary-name>.rs"
   ```
3. The main crate's `src/bin/<sidecar-binary-name>.rs` is a 5-line shim: `trusty_<sidecar>::run().await`.
4. The main crate depends on the sidecar library crate via `[workspace.dependencies]`.
5. Any supervisor/discovery code falls back to `std::env::current_exe().parent().join("<sidecar-binary-name>")` — `cargo install` puts all bins from a single crate in the same directory.

### Sidecar publish rule (IMPORTANT)

**Sidecar lib crates MUST be published to crates.io.** Do NOT set `publish = false` on a sidecar whose lib is a dependency of a published main crate — Cargo's dependency resolver requires all lib deps to exist on crates.io when publishing the depending crate. The single-install convention means users don't `cargo install <sidecar>` directly, but the crate must still be on crates.io as a library.

Only set `publish = false` on crates that are **not** depended on by any published crate (e.g., internal tooling, workspace-only binaries).

When publishing a main crate for the first time (or after updating a sidecar), publish the sidecar lib first, wait for crates.io index propagation (~90s), then publish the main crate.

### License field for crates.io (ALL published crates)

crates.io requires either a valid SPDX identifier or a `license-file`. `Elastic-2.0` is NOT in the SPDX registry. Every Elastic-licensed crate MUST use:
```toml
license-file = "LICENSE"
```
NOT `license = "Elastic-2.0"` (will be rejected at publish time). MIT-licensed crates use `license = "MIT"` (that is SPDX-valid). Verify before publishing any new crate.

### [patch.crates-io] for sidecar crates

After publishing a sidecar lib, add (or update) its entry in the workspace root `Cargo.toml` `[patch.crates-io]` section so local builds continue to use the in-tree source:
```toml
[patch.crates-io]
trusty-embedderd = { path = "crates/trusty-embedderd" }
trusty-bm25-daemon = { path = "crates/trusty-bm25-daemon" }
```

The earlier rule "No [patch.crates-io] needed" applies only to strictly-internal crates that are never published. Published sidecar libs need the patch entry.

### Sidecar inventory (audit checklist)

When adding a new sidecar to any main crate, update this list. "Bundled binaries"
means the crate's declared `[[bin]]` targets — the things `cargo install <crate>`
actually puts on `PATH`. Audit it against the manifests, not against this table.

| Main crate (crates.io package) | Bundled binaries | Sidecar lib on crates.io |
|---|---|---|
| trusty-search | `trusty-search`, `trusty-embedderd` ✅ (PR #190) | `trusty-embedderd` v0.3.0 ✅ |
| trusty-memory | `trusty-memory`, `trusty-bm25-daemon` ✅ (PR #191), `trusty-memory-mcp-bridge` | `trusty-bm25-daemon` v0.1.0 ✅ (PR #214) |
| trusty-installer | `trusty-installer`, `tctl` | — |
| trusty-analyze | `trusty-analyze` (no sidecars; bin gated on the `http-server` feature, which is in `default`) | — |
| trusty-code | `tcode` (no sidecars) | — |
| `tga` (dir `crates/trusty-git-analytics`) | `tga` (no sidecars) | — |
| trusty-mpm | `tm`, `trusty-mpm` — two names, one target (`src/bin/tm/main.rs`), both gated on the `cli` feature, which is in `default` | n/a |

**`trusty-memory` ships three binaries, not two.** `trusty-memory-mcp-bridge`
(`src/bin/mcp_bridge.rs`) is a deprecation shim for pre-#914 users; `cargo install
trusty-memory` produces all three in one command. It was missing from this checklist
and the omission propagated into
`docs/research/tart-vm-testing-harness/02-design/01-vm-install-harness.md` §7.2
before being caught. A Single-Install gate cannot detect the loss of a sidecar it
has never heard of.

**`trusty-mpm` publishes to crates.io.** Its manifest has **no `publish` key**, so
cargo defaults to `publish = true`; it is live at **v1.0.2** (`cargo search
trusty-mpm`). The earlier "(feature-gated, publish=false)" parenthetical on this row
was wrong on both counts and is removed. What *is* feature-gated is internal to the
single binary: `cli = ["daemon", "tui", "telegram", "slack"]` — the daemon, TUI,
Telegram, and Slack surfaces are **features compiled into `tm`**, reached as
subcommands, not separate `[[bin]]` targets. There are no `trusty-mpm-daemon`,
`trusty-mpm-mcp`, `trusty-mpm-tui`, or `trusty-mpm-telegram` binaries in this crate;
those names exist on crates.io only as v0.0.0 placeholder reservations. The
`publish = false` crate in this family is **`trusty-mpm-gui`** (a separate Tauri
crate, installed separately per the Single-Install convention), which is what the
old parenthetical was probably remembering.

**Removed row: `open-mpm`.** No such crate exists under `crates/`. It was stale.

Crates with `[[bin]]` targets that are deliberately **not** on this checklist because
they are not main-crate/sidecar bundles: `trusty-agents` (`tagent`),
`trusty-channels` (`slack-mcp`, `telegram-mcp`), `trusty-common` (`tickets-mcp`,
`candle_metal_bench`), `trusty-console`, `trusty-embedderd-py`, `trusty-gworkspace`,
`trusty-kb`, `trusty-review`, `trusty-sld-lint`, and the `publish = false`
`trusty-code-gui`, `trusty-mpm-gui`, `trusty-publish-guard`, `trusty-agents-local`.
Add one here the moment it gains a second bundled binary.
