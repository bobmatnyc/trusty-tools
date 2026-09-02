# Workspace Package and Code Map

This page maps the workspace's human-facing documentation to the code that
defines each package. It deliberately does not copy package versions: run
`cargo metadata --no-deps --format-version 1` for the live package set,
versions, publishability, and targets.

## Sources of truth

- Root `Cargo.toml`: workspace membership and shared dependency policy.
- `crates/<directory>/Cargo.toml`: package name, version, publishability, and
  build targets.
- `crates/<directory>/src/`: implemented behavior and public Rust API.
- `crates/<directory>/README.md`: package entry point and runnable usage.
- `docs/specs/README.md`: catalog of workspace behavior contracts and their
  implementation owners.
- Source-level `# Spec References` blocks: the mechanically checked mapping
  from code back to governing spec sections.

## Top-level packages

| Package | Code | Primary targets | Documentation |
|---|---|---|---|
| `tc-services` | [`crates/tc-services/`](../../crates/tc-services/) | library | [crate README](../../crates/tc-services/README.md), [extended docs](../tc-services/README.md) |
| `trusty-agents` | [`crates/trusty-agents/`](../../crates/trusty-agents/) | library, `tagent` | [crate README](../../crates/trusty-agents/README.md), [extended docs](../trusty-agents/README.md) |
| `trusty-agents-common` | [`crates/trusty-agents-common/`](../../crates/trusty-agents-common/) | library | [crate README](../../crates/trusty-agents-common/README.md), [extended docs](../trusty-agents-common/README.md) |
| `trusty-agents-local` | [`crates/trusty-agents-local/`](../../crates/trusty-agents-local/) | `trusty-agents-local` | [crate README](../../crates/trusty-agents-local/README.md), [extended docs](../trusty-agents-local/README.md) |
| `trusty-analyze` | [`crates/trusty-analyze/`](../../crates/trusty-analyze/) | library, `trusty-analyze` | [crate README](../../crates/trusty-analyze/README.md), [extended docs](../trusty-analyze/README.md) |
| `trusty-audit` | [`crates/trusty-audit/`](../../crates/trusty-audit/) | library, `trusty-audit`, `taudit` | [crate README](../../crates/trusty-audit/README.md) |
| `trusty-channels` | [`crates/trusty-channels/`](../../crates/trusty-channels/) | library, `slack-mcp`, `telegram-mcp` | [crate README](../../crates/trusty-channels/README.md) |
| `trusty-code` | [`crates/trusty-code/`](../../crates/trusty-code/) | library, `tcode` | [crate README](../../crates/trusty-code/README.md), [extended docs](../trusty-code/README.md) |
| `trusty-code-gui` | [`crates/trusty-code-gui/`](../../crates/trusty-code-gui/) | library, `trusty-code-gui` | [crate README](../../crates/trusty-code-gui/README.md) |
| `trusty-code-tui` | [`crates/trusty-code-tui/`](../../crates/trusty-code-tui/) | library | [crate README](../../crates/trusty-code-tui/README.md) |
| `trusty-common` | [`crates/trusty-common/`](../../crates/trusty-common/) | library, supporting binaries | [crate README](../../crates/trusty-common/README.md), [extended docs](../trusty-common/README.md) |
| `trusty-console` | [`crates/trusty-console/`](../../crates/trusty-console/) | library, `trusty-console` | [crate README](../../crates/trusty-console/README.md), [extended docs](../trusty-console/README.md) |
| `trusty-cto-db` | [`crates/trusty-cto-db/`](../../crates/trusty-cto-db/) | library | [crate README](../../crates/trusty-cto-db/README.md), [extended docs](../trusty-cto-db/README.md) |
| `trusty-embedderd` | [`crates/trusty-embedderd/`](../../crates/trusty-embedderd/) | library; bundled binary target is owned by `trusty-search` | [crate README](../../crates/trusty-embedderd/README.md), [extended docs](../trusty-embedderd/README.md) |
| `trusty-embedderd-py` | [`crates/trusty-embedderd-py/`](../../crates/trusty-embedderd-py/) | library, `trusty-embedderd-py` | [crate README](../../crates/trusty-embedderd-py/README.md) |
| `tga` | [`crates/trusty-git-analytics/`](../../crates/trusty-git-analytics/) | library, `tga` | [crate README](../../crates/trusty-git-analytics/README.md), [extended docs](../trusty-git-analytics/README.md) |
| `trusty-gworkspace` | [`crates/trusty-gworkspace/`](../../crates/trusty-gworkspace/) | library, `trusty-gworkspace-mcp` | [crate README](../../crates/trusty-gworkspace/README.md), [extended docs](../trusty-gworkspace/README.md) |
| `trusty-installer` | [`crates/trusty-installer/`](../../crates/trusty-installer/) | library, `trusty-installer`, `tctl` | [crate README](../../crates/trusty-installer/README.md) |
| `trusty-kb` | [`crates/trusty-kb/`](../../crates/trusty-kb/) | library, `trusty-kb` | [crate README](../../crates/trusty-kb/README.md) |
| `trusty-mcp` | [`crates/trusty-mcp/`](../../crates/trusty-mcp/) | library | [crate README](../../crates/trusty-mcp/README.md) |
| `trusty-memory` | [`crates/trusty-memory/`](../../crates/trusty-memory/) | library, `trusty-memory`, compatibility bridge | [crate README](../../crates/trusty-memory/README.md), [extended docs](../trusty-memory/README.md) |
| `trusty-mpm` | [`crates/trusty-mpm/`](../../crates/trusty-mpm/) | library, `tm`, `trusty-mpm` | [crate README](../../crates/trusty-mpm/README.md), [extended docs](../trusty-mpm/README.md) |
| `trusty-mpm-gui` | [`crates/trusty-mpm-gui/`](../../crates/trusty-mpm-gui/) | library, `trusty-mpm-gui` | [crate README](../../crates/trusty-mpm-gui/README.md) |
| `trusty-progress` | [`crates/trusty-progress/`](../../crates/trusty-progress/) | library | [crate README](../../crates/trusty-progress/README.md) |
| `trusty-publish-guard` | [`crates/trusty-publish-guard/`](../../crates/trusty-publish-guard/) | library, `publish-guard` | [crate README](../../crates/trusty-publish-guard/README.md) |
| `trusty-review` | [`crates/trusty-review/`](../../crates/trusty-review/) | library, `trusty-review` | [crate README](../../crates/trusty-review/README.md) |
| `trusty-search` | [`crates/trusty-search/`](../../crates/trusty-search/) | library, `trusty-search`, bundled `trusty-embedderd` | [crate README](../../crates/trusty-search/README.md), [extended docs](../trusty-search/README.md) |
| `trusty-sld-lint` | [`crates/trusty-sld-lint/`](../../crates/trusty-sld-lint/) | library, `sld-lint` | [crate README](../../crates/trusty-sld-lint/README.md) |

## Nested workspace packages

The root manifest explicitly includes these packages because `crates/*` does
not reach nested Tauri manifests.

| Package | Code | Primary target | Documentation |
|---|---|---|---|
| `trusty-agents-ui` | [`crates/trusty-agents/ui/src-tauri/`](../../crates/trusty-agents/ui/src-tauri/) | `trusty-agents-ui` | [UI README](../../crates/trusty-agents/ui/README.md) |
| `trusty-audit-ui` | [`crates/trusty-audit/ui/src-tauri/`](../../crates/trusty-audit/ui/src-tauri/) | `trusty-audit-ui` | [UI README](../../crates/trusty-audit/ui/README.md) |

## Consolidated former packages

The formerly separate `trusty-symgraph`, `trusty-rpc`, `trusty-tickets`,
`trusty-mcp-core`, `trusty-embedder`, `trusty-memory-core`, and
`trusty-monitor-tui` directories no longer exist. Their surviving capabilities
live primarily behind `trusty-common` features. MCP protocol primitives were
later extracted again into the standalone `trusty-mcp` package by
[ADR-0040](../adr/0040-trusty-mcp-services-absorbs-trusty-gworkspace.md).
There is no `trusty_common::mcp` compatibility re-export; the memory client is
`trusty_common::memory_rpc`.

## Runtime relationships

Cargo metadata describes compile-time edges only. The most important runtime
relationships are:

- `trusty-analyze` reads indexed corpora from a running `trusty-search` service.
- `trusty-mpm` provisions `trusty-search` and `trusty-memory` into managed
  sessions; the installer's dependency graph encodes those requirements.
- `trusty-review` uses `trusty-search` and invokes `trusty-analyze` on demand for
  review context.
- `trusty-console` discovers local services and presents their UI and health
  surfaces without becoming their behavior owner.

Check the owning crate README and source before changing any runtime edge.
