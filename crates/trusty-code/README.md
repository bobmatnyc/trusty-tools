# trusty-code

**Harness role:** The **Coding Harness** — per-project, Claude-Code-compatible
MPM orchestration. See
[docs/architecture/harnesses.md](../../docs/architecture/harnesses.md) for the
full three-harness architecture and delegation graph.

Why: Each project needs a harness that is already wired to its own `.claude/`
configuration — agents, skills, MCP connections, `CLAUDE.md`, and permissions.
`trusty-code` fills that role. It is the Claude-Code-native orchestration entry
point that runs the PM main-loop, enforces the mandatory workflow (research →
plan → implement → verify), and delegates authority to typed coding sub-agents
according to MPM protocols.

What: Per-project coding orchestration harness. One `tcode serve` process per
`.claude/` project root. Accepts task requests from CLI clients, TUI frontends,
and MCP callers. An original coding harness, tracked under epic #2052.

## Installation

### From GitHub Releases (recommended for binary users)

Prebuilt binaries are available for macOS (Apple Silicon) and Linux (x86_64).

1. Open [GitHub Releases](https://github.com/bobmatnyc/trusty-tools/releases),
   choose the newest `trusty-code-v<version>` release, and download the
   archive for your platform.

2. Extract and install:
   ```bash
   tar xzf trusty-code-*.tar.gz
   chmod +x tcode
   sudo mv tcode /usr/local/bin/    # or ~/.local/bin/ if you prefer user install
   ```

3. Verify the installation:
   ```bash
   tcode --version
   ```

### From Source with Cargo

Requires Rust 1.94 or later ([install Rust](https://rustup.rs/)).

```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-code --locked
```

This builds from the latest commit on `main` and installs the binary to `~/.cargo/bin/`. Make sure `~/.cargo/bin/` is on your PATH.

To install a specific release, replace `<version>` with its tag version:
```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools --tag trusty-code-vx.y.z trusty-code --locked
```

### With Homebrew (planned — not yet available)

```bash
brew tap bobmatnyc/trusty
brew install trusty-code
```

This installation method is under development. For now, use GitHub Releases or `cargo install`.

Once available, this will provide:
- Automatic updates via `brew upgrade trusty-code`
- Standard macOS / Linux PATH integration
- Optional dependency resolution (e.g., system libraries for ONNX Runtime)

### Prerequisites & Special Cases

#### Prerequisites

- **Claude Code** (optional but recommended): this is a per-project orchestration harness that integrates with Claude Code's internal agent APIs. Standalone usage is not yet documented.
- **Git**: standard; the tool reads git metadata for branch context.

Configuration and usage details are documented in the `trusty-code` crate README.

### Verify Installation

All installations can be verified by running:

```bash
tcode --version
```

The output includes the installed semantic version and build provenance.

## Status

`tcode` is an active per-project orchestration harness. Its daemon supports
stdio and HTTP JSON-RPC transports; the CLI drives task, session, transcript,
workstream, and TUI flows. `run-workflow` remains the incomplete surface.

## Binaries

| Binary | Description |
|--------|-------------|
| `tcode` | Per-project Claude-Code-compatible MPM orchestration harness |

## Primary subcommands

| Subcommand | Description |
|------------|-------------|
| `tcode serve [--project <PATH>] --stdio\|--http` | Start a project-bound or projectless orchestration server |
| `tcode tui [--project <PATH>]` | Launch the interactive TUI and attach to or start an HTTP daemon |
| `tcode run-task <agent> <task>` | Run a task through a daemon-owned session |
| `tcode session …` | List, inspect, and create sessions |
| `tcode attach`, `cancel`, `transcript` | Operate on an existing session |
| `tcode workstream …` | Manage workstreams and their active state |
| `tcode run-workflow <name>` | Reserved workflow runner; not yet implemented |

## Build

```bash
cargo build -p trusty-code
cargo run -p trusty-code -- --version
cargo test -p trusty-code --no-fail-fast
```

## Design Constraints

- **Claude-Code compatible** — reads `.claude/` config, agents, skills, MCP
  descriptors, `CLAUDE.md`, and permission grants exactly as Claude Code does.
- **Per-agent model routing** — each agent may specify its own model
  (AWS Bedrock or OpenRouter).
- **Single-instance per project** — one `tcode serve` process per `.claude/`
  root.
- **Event-driven** — publishes daemon-owned session and task events to attached
  clients.

## Architecture Role

trusty-code is the bottom layer of the three-harness stack:

```
trusty-agents (general agentic)  →  delegates coding tasks to trusty-code
trusty-mpm (meta-harness)        →  launches and oversees trusty-code sessions
trusty-code (coding harness)     →  executes per-project coding workflows
```

See [docs/architecture/harnesses.md](../../docs/architecture/harnesses.md) and
[ADR-0004](../../docs/adr/0004-three-harnesses-shared-event-driven-common.md).
