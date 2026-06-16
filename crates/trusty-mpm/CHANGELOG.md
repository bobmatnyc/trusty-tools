# Changelog — trusty-mpm

## [0.8.2] — 2026-06-16

### Changed (closes part of #1318)

- **De-bundled `trusty-console`.** Removed the bundled `trusty-console`
  `[[bin]]` shim and dependency. `cargo install trusty-mpm` now produces
  `tm` and `trusty-mpm` only. Install the console with
  `cargo install trusty-console`. This is part of the single-owner-per-binary
  fix for the cargo binary-ownership collisions (#1262).

## [Unreleased]

### Deprecated: verbose managed session-lifecycle verbs (issue #1205)

The managed session-lifecycle CLI verbs were renamed to the cleaner, symmetric
`stop` / `resume` / `decommission` family. The old verbose verbs still work but
now emit a one-line deprecation notice to **stderr** on every invocation and
will be removed in a future release.

| Deprecated verb | Use instead | Behavior |
|-----------------|-------------|----------|
| `tm session runtime-stop <id>` | `tm session stop <id>` | Stop the runtime, keep the workspace (resumable) |
| `tm session managed-stop <id>` | `tm session stop <id>` | Same as `runtime-stop` |
| `tm session managed-resume <id>` | `tm session resume <id>` | Re-spawn the runtime in the existing workspace |

- The deprecated verbs are hidden from `tm session --help` but continue to parse
  for backward compatibility.
- Each deprecated invocation prints `warning: '<old>' is deprecated; use '<new>'`
  to stderr; stdout stays clean for scripts.
- `tm session decommission <id>` (terminal teardown: remove workspace from disk)
  is unchanged.

## [0.5.0] — 2026-05-28

### Added: `tm services` — canonical service-discovery CLI (issue #339)

**New subcommand**: `tm services <action>` — replaces ad-hoc `lsof`/`curl`/`ps`
patterns for discovering the port, health, and status of every trusty-* daemon.

#### Subcommands

| Command | Description |
|---------|-------------|
| `tm services list [--json]` | Table of all declared services with running/down status, port, version, and health |
| `tm services status <name> [--json]` | Detailed block for one service |
| `tm services port <name>` | Print just the port number (scriptable) |
| `tm services url <name>` | Print the full base URL |
| `tm services health <name>` | Probe the `/health` endpoint; exit 0 if healthy |
| `tm services log <name>` | Print the log file path if it exists |
| `tm services init [--force]` | Write the default manifest to `~/.claude-mpm/services.yaml` |
| `tm services restart <name>` | Execute the manifest `restart_cmd` |

#### Manifest

Default manifest embedded in the binary covers 6 services:

- `trusty-search` — port 7878, `/health` confirmed
- `trusty-analyze` — port 7879, `/health` confirmed
- `trusty-mpm-daemon` — port 7880, `/health` confirmed at `daemon/api.rs:74`
- `trusty-memory` — dynamic port (7070-7079) via `~/.trusty-memory/http_addr`
- `trusty-embedderd` — UDS sidecar, pgrep-only (no HTTP surface)
- `trusty-bm25-daemon` — UDS sidecar, pgrep-only (no HTTP surface)

Custom manifests can be placed at `~/.claude-mpm/services.yaml` (use `tm services init`).

#### Exit codes

| Code | Meaning |
|------|---------|
| 0 | Running/healthy (list always exits 0) |
| 1 | Service declared but down, or health probe failed |
| 2 | Service name not in manifest |

#### Scriptable usage

```bash
PORT=$(tm services port trusty-search)
URL=$(tm services url trusty-search)
tail -f $(tm services log trusty-search)
```

#### Architecture

- `crates/trusty-mpm/src/services/manifest.rs` — `ServicesManifest`, `ServiceDecl`,
  `PortDiscovery` enum, `ManifestValidationError` (thiserror)
- `crates/trusty-mpm/src/services/discoverer.rs` — `Discoverer` with 5-second
  TTL cache; `ProcessProber`/`PortProber`/`HttpProber`/`VersionRunner` trait
  seams for unit testing
- `crates/trusty-mpm/assets/default-services.yaml` — embedded default manifest

**Tests**: 21 new unit tests (8 manifest + 13 discoverer, all mocked) + 11 CLI
parse tests + 2 ignore-gated integration smoke tests.

---

## [consolidation] — 2026-05-26

**Combined 7 trusty-mpm-\* sub-crates into one crate with feature-gated `[[bin]]` targets.**

### Summary

The following sub-crates have been merged into this unified `trusty-mpm` crate:

| Former crate | Now lives in |
|---|---|
| `trusty-mpm-core` | `crates/trusty-mpm/src/core/` |
| `trusty-mpm-client` | `crates/trusty-mpm/src/client/` |
| `trusty-mpm-mcp` | `crates/trusty-mpm/src/mcp/` (feature: `mcp`) |
| `trusty-mpm-daemon` | `crates/trusty-mpm/src/daemon/` (feature: `daemon`) |
| `trusty-mpm-cli` | `crates/trusty-mpm/src/bin/tm.rs` (feature: `cli`) |
| `trusty-mpm-tui` | `crates/trusty-mpm/src/tui/` (feature: `tui`) |
| `trusty-mpm-telegram` | `crates/trusty-mpm/src/telegram/` (feature: `telegram`) |

The Tauri desktop GUI (`trusty-mpm-gui`) remains as a separate crate because
it owns `build.rs` (invoking `tauri_build::build()`) and `tauri.conf.json` — files
that cannot co-exist with a generic Cargo crate build system. The `gui` feature of
this crate wraps it as an optional path dependency.

### Workspace crate count
- Removed: 7 crates (`trusty-mpm-core`, `trusty-mpm-mcp`, `trusty-mpm-daemon`,
  `trusty-mpm-client`, `trusty-mpm-cli`, `trusty-mpm-tui`, `trusty-mpm-telegram`)
- Added: 1 crate (`trusty-mpm`)
- Net change: 28 → 22 workspace members

### Feature flags

| Feature | What it enables |
|---|---|
| `default` | `cli` + `daemon` (the common install path) |
| `cli` | `tm` / `trusty-mpm` CLI binary (implies `daemon`, `tui`, `telegram`) |
| `daemon` | `trusty-mpmd` daemon binary + daemon library module (implies `mcp`) |
| `mcp` | MCP server library module |
| `tui` | `trusty-mpm-tui` shim binary + TUI library module |
| `telegram` | `trusty-mpm-telegram` shim binary + Telegram library module |
| `gui` | `trusty-mpm-gui` shim binary (wraps the separate `trusty-mpm-gui` crate) |

### Public API surface

All public types, traits, and functions are preserved. The only change is the
import path: code that previously imported from `trusty_mpm_core`, `trusty_mpm_client`,
etc. should now import from the corresponding submodule of `trusty_mpm`:

```rust
// Before
use trusty_mpm_core::session::{Session, SessionId};
use trusty_mpm_client::DaemonClient;

// After
use trusty_mpm::core::session::{Session, SessionId};
use trusty_mpm::client::DaemonClient;
```

### Deprecation notes

The following crate names are no longer published:
- `trusty-mpm-core`
- `trusty-mpm-mcp`
- `trusty-mpm-daemon`
- `trusty-mpm-client`
- `trusty-mpm-cli`
- `trusty-mpm-tui`
- `trusty-mpm-telegram`

All functionality is available under `trusty-mpm` with the appropriate feature flags.

## [0.4.0] and prior

See the individual crate changelogs in the former sub-crate directories (available
in git history as `crates/trusty-mpm-{core,client,mcp,daemon,cli,tui,telegram}/`).
