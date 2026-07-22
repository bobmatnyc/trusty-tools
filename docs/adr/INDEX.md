# ADR Index — Accepted Decisions

**Last updated:** 2026-07-22 | **Format version:** 1.0

This index is the **single source of truth** for the ADR corpus. It serves as a quick reference for **consistency vetting** (see DOC-46 §3, "Related Decisions" protocol) and as a discoverability surface for understanding the architectural decision landscape.

## All ADRs

| # | Title | Status | One-line decision | Scope |
|---|---|---|---|---|
| [0001](./0001-docs-live-top-level.md) | Design/research/ADR docs live in top-level `docs/` | Accepted | All documentation lives in `docs/`, not scattered | Workspace |
| [0002](./0002-single-install-convention.md) | Single-install convention for main crates | Accepted | All major crates install to the same location via single `cargo install` command | Workspace |
| [0003](./0003-msrv-and-edition-policy.md) | MSRV 1.88 and per-crate Rust edition policy | Accepted | MSRV drives Rust edition choice; crates use 2021 or 2024 per feature support | Workspace |
| [0004](./0004-three-harnesses-shared-event-driven-common.md) | Three distinct harnesses on a shared event-driven trusty-common foundation | Accepted | Three independent harnesses (trusty-agents, trusty-search, trusty-memory) share event-driven, KG-backed trusty-common | Workspace |
| [0005](./0005-harness-event-bus.md) | Shared HarnessEvent envelope + process-global event bus in trusty-agents-common | Superseded by 0019 | Unified event bus for all harness events; subscribers do not know message types in advance ("adapt, don't fold") | Workspace |
| [0006](./0006-trusty-controller-naming.md) | Name the stack control plane `trusty-controller` (binary `tctl`) | Superseded by 0013 | Control plane service is called trusty-controller; CLI binary is `tctl` (replaced by trusty-installer in ADR-0013) | Workspace |
| [0007](./0007-tool-contract-versioning-and-verb-model.md) | Monotonic-integer `contract_version` + 3-layer extensible verb model | Accepted | Tools versioned by contract_version integer; verbs have Base/Extended/Custom layers for extensibility | Workspace |
| [0008](./0008-project-identity-convention.md) | Project-identity convention: full-path slug of the nearest git root | Accepted | Projects identified by full-path slug of nearest `.git` root (e.g., `/Users/alice/dev/myapp` → `/Users/alice/dev/myapp`) | Workspace |
| [0009](./0009-external-extractor-kg-ingest-contract.md) | External-extractor KG ingest contract: durable contributed overlay in trusty-search | Accepted | External extractors contribute KG subgraphs; trusty-search merges them into main KG as durable overlays | Workspace |
| [0010](./0010-kg-edge-kind-extensibility.md) | KG edge-kind extensibility: first-class data-flow variants + Custom escape hatch | Accepted | KG supports standard edge kinds (depends, defines, calls, …) plus Custom for user-defined variants | Workspace |
| [0011](./0011-tctl-owns-service-lifecycle.md) | `tctl` headless control plane (owns boot/lifecycle); `trusty-console` is the single HTTP surface | Amended by 0018 | tctl manages daemon lifecycle (start/stop/status); trusty-console is the single HTTP API surface for all user interactions | Workspace |
| [0012](./0012-per-instance-guid-and-marker-file-identity.md) | Per-instance GUID and marker-file identity | Accepted | Each daemon instance has a GUID; identity is marked on disk by a marker file (e.g., `.trusty-search/INSTANCE_ID`) | Workspace |
| [0013](./0013-rename-trusty-controller-to-trusty-installer.md) | Rename `trusty-controller` → `trusty-installer`; add `tctl` transitional alias; build out interactive installer | Accepted | trusty-controller renamed trusty-installer; `tctl` becomes transitional alias; interactive installer built out | Workspace |
| [0014](./0014-native-mcp-support.md) | Ship full native MCP support (ticketing, gworkspace, Slack/Telegram, and more) | Accepted | Consolidate MCP framework in trusty-common; ship native MCP servers for all integrations | Workspace |
| [0015](./0015-three-product-agent-composition-model.md) | Unified agent composition: shared `.md`+YAML+`extends` format across trusty-agents, trusty-mpm, trusty-code | Proposed | Single agent-composition format across all three orchestration engines | Workspace |
| [0016](./0016-orchestration-hierarchy-lead-pm-assistant.md) | Orchestration Hierarchy: Engineering Lead / PM / Assistant | Proposed | Three-tier agent orchestration hierarchy: Engineering Lead leads cross-tool workstreams; PM orchestrates projects; Assistant executes delegated tasks | Workspace |
| [0017](./0017-shared-ingress-via-console-tailscale-funnel.md) | Shared webhook ingress via trusty-console + Tailscale Funnel | Proposed | External webhooks flow through one /api/webhooks/{source} endpoint, reverse-proxied by trusty-console, exposed publicly via Tailscale Funnel | Workspace |
| [0018](./0018-loopback-only-doctrine.md) | Loopback-only doctrine: `trusty-console` is the sole off-loopback HTTP surface | Accepted | trusty-console is the only daemon allowed to bind off-loopback; sibling daemons may run loopback-only HTTP for CLI/stdio-bridge/GUI use (amends 0011) | Workspace |
| [0019](./0019-unified-ipc-messaging-on-event-bus.md) | Unified IPC messaging on the event-driven control bus | Accepted | Single durable IPC channel for all cross-PM and cross-agent messaging, built on the event bus with explicit delivery acknowledgment | Workspace |
| [0020](./0020-session-owned-worktrees.md) | Session-owned worktrees: ownership registry + owner-gated reclamation | Accepted | Worktree sentinels + `SessionRecord.worktree_owner` registry field record an owning session; orphan-GC and `decommission` never reclaim an owner-unknown or live-owned worktree; zero migration for legacy worktrees | crate `trusty-mpm` |

## Notes

- **ADRs 0001–0013** were grandfathered as Accepted without re-vetting when DOC-46 (this spec) was introduced.
- **Future ADRs** (0014+) must include a "Related Decisions" section (DOC-46 §3) documenting consistency vetting before acceptance.
- **Crate-specific decisions** (if a crate maintains `docs/<crate>/decisions/`) are tracked in that crate's own index.
- **Superseded ADRs** remain in this index with status "Superseded by NNNN" for historical reference.
