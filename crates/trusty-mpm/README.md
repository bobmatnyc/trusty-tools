# trusty-mpm

Why: A single `cargo install trusty-mpm` should install all trusty-mpm tooling — the background daemon, the CLI, the TUI dashboard, and the Telegram bot — without requiring users to install eight separate packages and coordinate their versions.

What: Unified crate combining the formerly separate `trusty-mpm-{core,client,mcp,daemon,cli,tui,telegram}` sub-crates into one package with feature-gated `[[bin]]` targets.

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
| `cli` | `tm` / `trusty-mpm` CLI binary | `daemon`, `tui`, `telegram` |
| `daemon` | `trusty-mpmd` daemon binary + library | `mcp` |
| `mcp` | MCP server library module | — |
| `tui` | `trusty-mpm-tui` shim + TUI library | ratatui, crossterm |
| `telegram` | `trusty-mpm-telegram` shim + bot library | teloxide |
| `gui` | `trusty-mpm-gui` shim binary | trusty-mpm-gui (Tauri) |

## Binaries

| Binary | Feature | Description |
|---|---|---|
| `tm` / `trusty-mpm` | `cli` | Unified CLI: daemon control, sessions, projects, TUI, Telegram, MCP |
| `trusty-mpmd` | `daemon` | Background daemon: HTTP API, hook relay, session registry |
| `trusty-mpm-tui` | `tui` | Backward-compatible TUI shim (prefer `tm tui`) |
| `trusty-mpm-telegram` | `telegram` | Backward-compatible Telegram bot shim (prefer `tm telegram`) |
| `trusty-mpm-gui` | `gui` | Backward-compatible GUI shim (prefer `tm gui`) |

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
│       ├── tm.rs                  # CLI entry point
│       ├── trusty-mpmd.rs         # Daemon shim
│       ├── trusty-mpm-tui.rs      # TUI shim
│       ├── trusty-mpm-telegram.rs # Telegram shim
│       └── trusty-mpm-gui.rs      # GUI shim
└── tests/
    ├── e2e/             # HTTP-level daemon integration tests
    └── test_session_lifecycle.rs
```
