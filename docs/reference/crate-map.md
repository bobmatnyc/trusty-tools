# Per-Crate Reference Map

Detailed implementation information for each crate lives in its own documentation:

## Shared Libraries

- **trusty-common** — see `crates/trusty-common/README.md` and `docs/trusty-common/`
- **trusty-embedderd** — see `crates/trusty-embedderd/README.md` and `docs/trusty-embedderd/` (fastembed sidecar daemon)
- **trusty-bm25-daemon** — see `crates/trusty-bm25-daemon/README.md` and `docs/trusty-bm25-daemon/` (BM25 index sidecar)

## Published MCP Servers & Daemons

- **trusty-search** — see `crates/trusty-search/README.md` and **`docs/trusty-search/`** (primary worked example with regression testing, research, sessions)
- **trusty-memory** — see `crates/trusty-memory/README.md` and `docs/trusty-memory/` (licensed MIT, not Elastic-2.0; storage engine lives in `trusty-common`'s `memory-core` feature)
- **trusty-analyze** — see `crates/trusty-analyze/README.md` and `docs/trusty-analyze/` (licensed MIT, not Elastic-2.0)
- **trusty-mpm** — see `crates/trusty-mpm/README.md` and `docs/trusty-mpm/` (unified platform: CLI binaries `tm`/`trusty-mpm`, daemon, MCP server, TUI, Telegram)
- **trusty-git-analytics** — see `crates/trusty-git-analytics/README.md` and `docs/trusty-git-analytics/`

## Service Layers & Supporting Crates

- **trusty-gworkspace** — Google Workspace client (Calendar, Tasks, Drive)
- **trusty-cto-db** — SQLite CTO database (rusqlite-backed)
- **tc-services** — service-layer adapters: CTO DB, Granola, GWorkspace

## Agent Orchestration Platform

- **trusty-agents** — see `crates/trusty-agents/README.md` and `docs/trusty-agents/` (agent orchestration platform, bin: `tagent`)
- **trusty-agents-common** — see `crates/trusty-agents-common/README.md` (common API types for trusty-agents, publish=false)
- **trusty-agents-local** — see `crates/trusty-agents-local/README.md` (local execution engine for trusty-agents, publish=false)

## Desktop & CLI Applications

- **trusty-mpm-gui** — see `crates/trusty-mpm-gui/README.md` (Tauri desktop GUI, publish=false)
- **cto-assistant** — CTO assistant CLI (publish=false)
- **trusty-code** — per-project Claude-Code-compatible MPM orchestration harness (bin: `tcode`); Phase 0 scaffold; extraction tracked in #587

## License Information

Most crates are **Elastic License 2.0**, but `trusty-memory`, `trusty-analyze`, and a few others are **MIT**. Check each crate's `Cargo.toml` for definitive licensing information.

## Consolidated Modules in trusty-common

The following formerly-separate crates are now consolidated into `trusty-common` behind feature flags. Enable the relevant feature to use:

- `symgraph` — graph symbol indexing
- `rpc` — RPC infrastructure
- `tickets` — issue/ticket tracking
- `mcp` — MCP server foundations
- `embedder` — embedding pipeline
- `memory-core` — memory palace storage engine
- `monitor-tui` — monitoring TUI
