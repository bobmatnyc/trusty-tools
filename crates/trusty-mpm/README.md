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

### From GitHub Releases (recommended for binary users)

Prebuilt binaries are available for macOS (Apple Silicon) and Linux (x86_64).

1. Download the latest release from [GitHub Releases](https://github.com/bobmatnyc/trusty-tools/releases):
   - Look for assets tagged `trusty-mpm-v0.6.2`
   - Download the archive for your platform:
     - **macOS arm64 (Apple Silicon)**: `trusty-mpm-v0.6.2-aarch64-apple-darwin.tar.gz`
     - **Linux x86_64**: `trusty-mpm-v0.6.2-x86_64-unknown-linux-gnu.tar.gz`

2. Extract and install:
   ```bash
   tar xzf trusty-mpm-v0.6.2-*.tar.gz
   chmod +x tm trusty-mpm
   sudo mv tm trusty-mpm /usr/local/bin/    # or ~/.local/bin/ if you prefer user install
   ```

3. Verify the installation:
   ```bash
   tm --version
   ```

### From Source with Cargo

Requires Rust 1.91 or later ([install Rust](https://rustup.rs/)).

```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-mpm --locked
```

This builds from the latest commit on `main` and installs the binaries (`tm` and `trusty-mpm`) to `~/.cargo/bin/`. Make sure `~/.cargo/bin/` is on your PATH.

To install a specific version:
```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools --tag trusty-mpm-v0.6.2 trusty-mpm --locked
```

### With Homebrew (planned — not yet available)

```bash
brew tap bobmatnyc/trusty
brew install trusty-mpm
```

This installation method is under development. For now, use GitHub Releases or `cargo install`.

Once available, this will provide:
- Automatic updates via `brew upgrade trusty-mpm`
- Standard macOS / Linux PATH integration
- Optional dependency resolution (e.g., system libraries for ONNX Runtime)

### Prerequisites & Special Cases

#### System Requirements

- **Node.js** (optional): only needed if you plan to use the MPM JavaScript SDK or integrate with third-party JavaScript tooling. The daemon and CLI work independently of Node.
- **OS**: macOS 12+ or Linux. Windows support is not yet available.

#### Binaries Installed

This crate installs two binaries with the same functionality:
- `tm` — shorter alias for quick access
- `trusty-mpm` — canonical name

Both are identical; choose whichever you prefer. The CLI (`tm` / `trusty-mpm`) discovers the running daemon automatically via the standard socket or HTTP port and requires no configuration beyond a running daemon.

#### Configuration

The daemon reads from `~/.config/trusty-mpm/config.yaml` by default. See the `trusty-mpm` crate README for configuration examples and the full option reference.

```bash
tm daemon --config /path/to/config.yaml
```

### Verify Installation

All installations can be verified by running:

```bash
tm --version
```

Expected output: the semantic version of the installed binary (e.g., `tm 0.6.2`).

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
