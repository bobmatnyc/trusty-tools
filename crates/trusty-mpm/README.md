# trusty-mpm

**Harness role:** The **Meta-Harness** — PM-style multi-agent orchestration
over coding work. Manages multi-project sessions, relays hooks, and exposes an
MCP server to Claude Code sessions. Delegates coding tasks to `trusty-code`
(`tcode`). See
[docs/architecture/harnesses.md](../../docs/architecture/harnesses.md) for the
full three-harness architecture and delegation graph.

Why: A single `cargo install trusty-mpm` should install all trusty-mpm tooling — the background daemon, the CLI, the TUI dashboard, and the Telegram bot — without requiring users to install eight separate packages and coordinate their versions.

What: Unified crate combining the formerly separate `trusty-mpm-{core,client,mcp,daemon,cli,tui,telegram}` sub-crates into one package with a single `[[bin]]` target (`tm` / `trusty-mpm`) that exposes all functionality via subcommands.

## Installation

```bash
# Install the common toolset (CLI + daemon):
cargo install trusty-mpm

# Install with TUI and Telegram bot:
cargo install trusty-mpm --features tui,telegram

# Install just the library (no binaries):
cargo add trusty-mpm --no-default-features
```

## Feature Flags

| Feature | Description | Enables |
|---|---|---|
| `default` | Common install: CLI + daemon | `cli`, `daemon` |
| `cli` | `tm` / `trusty-mpm` binary | `daemon`, `tui`, `telegram` |
| `daemon` | Daemon library module + `daemon` subcommand | `mcp` |
| `mcp` | MCP server library module | — |
| `tui` | TUI library module (`tm tui`) | ratatui, crossterm |
| `telegram` | Telegram bot library module (`tm telegram`) | teloxide |
| `gui` | `tm gui` subcommand (non-default; pulls in Tauri/WebKit) | trusty-mpm-gui (Tauri) |

## Binary

There is a single binary with two install names: `tm` (short alias) and `trusty-mpm` (canonical name), both compiled from the same source.

| Binary | Feature | Description |
|---|---|---|
| `tm` / `trusty-mpm` | `cli` | Unified CLI: daemon control, sessions, projects, TUI, Telegram, MCP |

All functionality previously in standalone shim binaries (`trusty-mpmd`, `trusty-mpm-tui`,
`trusty-mpm-telegram`, `trusty-mpm-gui`) is now accessed through subcommands:
`trusty-mpm daemon`, `trusty-mpm tui`, `trusty-mpm telegram`, `trusty-mpm gui`.

## Library Modules

```
trusty_mpm::core       // Domain types: agents, sessions, hooks, IPC protocol
trusty_mpm::client     // Daemon HTTP client, command model, executor
trusty_mpm::mcp        // MCP server: OrchestratorBackend trait + dispatch  (feature: mcp)
trusty_mpm::daemon     // Daemon library: API router, DaemonState, serve_http  (feature: daemon)
trusty_mpm::tui        // ratatui coordinator dashboard  (feature: tui)
trusty_mpm::telegram   // Telegram bot library  (feature: telegram)
```

## Quick Start

```bash
# Start the daemon
tm start

# Check status
tm status

# Launch a Claude Code session in the current directory
tm launch

# Open the TUI dashboard
tm tui

# Pair a Telegram bot
tm telegram pair
```

## GUI Note

The Tauri desktop GUI lives in the separate `trusty-mpm-gui` crate (it owns
`build.rs` + `tauri.conf.json` which require Tauri's build system). The `gui`
feature of this crate wraps it as an optional dependency. Building the GUI
requires the Tauri prerequisites: `xcode-select`, `rustup`, `pnpm`.

## Upgrading / Migration

The standalone shim binaries `trusty-mpmd`, `trusty-mpm-tui`, `trusty-mpm-telegram`,
and `trusty-mpm-gui` have been removed. Their functionality is now exposed through
subcommands of the single `trusty-mpm` (or `tm`) binary:

| Old binary | New command |
|---|---|
| `trusty-mpmd [--addr <addr>]` | `trusty-mpm daemon [--addr <addr>]` |
| `trusty-mpm-tui` | `trusty-mpm tui` |
| `trusty-mpm-telegram` | `trusty-mpm telegram` |
| `trusty-mpm-gui` | `trusty-mpm gui` (requires `--features gui`) |

**Update any external references** (launchd LaunchAgent plists, systemd units,
Docker `CMD`, shell aliases, CI scripts) that reference `trusty-mpmd` to use
`trusty-mpm daemon --addr <addr>` instead. No symlink or wrapper shim is
provided — this is a clean break with a documented migration path.

Example launchd plist update:

```xml
<!-- Before -->
<string>/Users/you/.cargo/bin/trusty-mpmd</string>

<!-- After -->
<string>/Users/you/.cargo/bin/trusty-mpm</string>
<string>daemon</string>
<string>--addr</string>
<string>127.0.0.1:7880</string>
```

## Architecture

```
crates/trusty-mpm/
├── src/
│   ├── lib.rs           # Re-exports core::* and client::*
│   ├── core/            # Domain types (agents, sessions, hooks, IPC)
│   ├── client/          # HTTP client + command model
│   ├── mcp/             # MCP server protocol (feature: mcp)
│   ├── daemon/          # HTTP daemon (feature: daemon)
│   ├── tui/             # ratatui dashboard (feature: tui)
│   ├── telegram/        # Telegram bot (feature: telegram)
│   └── bin/
│       └── tm/          # Single CLI entry point (compiled as both `tm` and `trusty-mpm`)
│           ├── main.rs
│           ├── cli.rs
│           ├── commands/
│           ├── formatters/
│           └── types.rs
└── tests/
    ├── e2e/             # HTTP-level daemon integration tests
    └── test_session_lifecycle.rs
```
