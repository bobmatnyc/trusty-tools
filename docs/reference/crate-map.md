# Crate Map Reference

## Code Structure

```
trusty-tools/               # workspace root
├── Cargo.toml              # workspace manifest — glob members = ["crates/*"]
├── Cargo.lock
├── crates/                 # 20 members (matches `ls crates/`)
│   ├── trusty-common/       # shared utilities, tracing, OpenRouter chat; hosts the
│   │                        # consolidated rpc/embedder/symgraph/memory-core/
│   │                        # tickets/monitor-tui modules behind feature flags
│   ├── trusty-mcp/          # JSON-RPC 2.0 / MCP protocol primitives: envelopes, the
│   │                        # stdio dispatch loop, OpenRPC discovery, daemon bridge
│   ├── trusty-embedderd/    # fastembed wrapper — sidecar daemon for trusty-search
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
│   ├── trusty-audit/        # auditor client handed to a client company — installs its pinned
│   │                        # tools via trusty-installer and drives the audit workflow
│   │                        # (bins: trusty-audit, taudit alias); headless library + thin CLI,
│   │                        # publish = false; scaffold tracked in #5502, epic #5477
│   └── trusty-installer/    # install/upgrade orchestrator (bins: trusty-installer, tctl alias); ADR-0013 / SPEC-INSTALLER-01; RFC tracked in #920
└── .gitignore
```

> **Consolidation note:** the formerly separate `trusty-symgraph`, `trusty-rpc`,
> `trusty-tickets`, `trusty-mcp-core`, `trusty-embedder`, `trusty-memory-core`,
> and `trusty-monitor-tui` crates no longer exist as standalone directories —
> they were absorbed into `trusty-common` behind the `symgraph`, `rpc`,
> `tickets`, `mcp`, `embedder`, `memory-core`, and `monitor-tui` feature flags
> respectively. Enable the relevant feature to pull in the corresponding module.
>
> **`mcp` is the one that came back out.** ADR-0040 (#5803) re-extracted those
> primitives as the standalone `trusty-mcp` crate, because the consumer set is
> MCP servers rather than every trusty-* binary. `trusty_common::mcp` no longer
> exists and has no re-export shim; the trusty-memory JSON-RPC client it used to
> contain stayed behind as `trusty_common::memory_rpc` (`memory-rpc` feature).

For the source layout of any crate, read its `README.md` or browse
`crates/<name>/src/`. Each crate owns its own `README.md` covering purpose,
usage, and design notes.

## Per-Crate Reference

Detailed implementation information for each crate lives in its own documentation:

- **trusty-common** — see `crates/trusty-common/README.md` and `docs/trusty-common/`
- **trusty-mcp** — see `crates/trusty-mcp/README.md` (JSON-RPC 2.0 / MCP protocol primitives; ADR-0040)
- **trusty-embedderd** — see `crates/trusty-embedderd/README.md` and `docs/trusty-embedderd/` (fastembed sidecar daemon)
- **trusty-memory** — see `crates/trusty-memory/README.md` and `docs/trusty-memory/` (storage engine lives in `trusty-common`'s `memory-core` feature)
- **trusty-search** — see `crates/trusty-search/README.md` and **`docs/trusty-search/`** (primary worked example with regression testing, research, sessions)
- **trusty-analyze** — see `crates/trusty-analyze/README.md` and `docs/trusty-analyze/`
- **trusty-mpm** — see `crates/trusty-mpm/README.md` and `docs/trusty-mpm/` (unified platform: CLI binaries `tm`/`trusty-mpm`, daemon, MCP server, TUI, Telegram)
- **trusty-mpm-gui** — see `crates/trusty-mpm-gui/README.md` (Tauri desktop GUI, publish=false)
- **trusty-agents** — see `crates/trusty-agents/README.md` and `docs/trusty-agents/` (agent orchestration platform, bin: `tagent`)
- **trusty-agents-common** — see `crates/trusty-agents-common/README.md` (common API types for trusty-agents, publish=false)
- **trusty-agents-local** — see `crates/trusty-agents-local/README.md` (local execution engine for trusty-agents, publish=false)
- **trusty-git-analytics** — see `crates/trusty-git-analytics/README.md` and `docs/trusty-git-analytics/`
- **trusty-installer** — see `crates/trusty-installer/README.md` and `docs/trusty-installer/` (install/upgrade orchestrator, bins: `trusty-installer` + `tctl` transitional alias; ADR-0013; RFC #920; renamed from `trusty-controller` in #1757)

For license details, check each crate's `Cargo.toml`: all 27 `crates/*` glob members are **MIT**, inherited from the workspace root `[workspace.package]` section via `license.workspace = true`. The nested internal (unpublished) Tauri crate `trusty-agents-ui` (`crates/trusty-agents/ui/src-tauri`) does not declare a license field.
