# Crate Map Reference

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
