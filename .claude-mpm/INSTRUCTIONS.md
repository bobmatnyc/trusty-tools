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

### No [patch.crates-io] needed
Internal crates resolve via path deps. Never add `[patch.crates-io]` for internal crates.

### Required Workflow Sequence
(prompt → ticket) OR (check tickets) → read ticket → implement → test → build → version bump → publish → update consumers → verify CI

### Phase 0: Ticket
No work begins without a ticket reference.

### Phase 1: Read Ticket
Always read full ticket + comments before writing code.

### Phase 2: Implement
- Agent: rust-engineer (model: opus)
- No `unwrap()` in library code
- `thiserror` for crates, `anyhow` for binaries
- Why/What/Test doc pattern on every public item

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

### Sidecar inventory (audit checklist)

When adding a new sidecar to any main crate, update this list:

| Main crate | Bundled binaries |
|---|---|
| trusty-search | `trusty-search`, `trusty-embedderd` ✅ (PR #190) |
| trusty-memory | `trusty-memory`, `trusty-memory-mcp-bridge`, `trusty-bm25-daemon` ✅ (feat/trusty-memory-bundled-bm25-daemon-install) |
| trusty-analyze | `trusty-analyze` (audit needed) |
| trusty-git-analytics | `tga` (audit needed) |
| trusty-mpm | `tm`, `trusty-mpm-daemon`, `trusty-mpm-mcp`, `trusty-mpm-tui`, `trusty-mpm-telegram` (feature-gated bins) |
| open-mpm | `open-mpm` (audit needed) |
