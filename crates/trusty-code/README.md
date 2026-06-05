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
and MCP callers. Full extraction from `open-mpm` is tracked in epic #587.

## Status

**Phase 0 scaffold.** The `tcode` binary parses its CLI surface but every
subcommand stubs out with "not yet implemented". Implementation phases are
tracked in epic #587.

## Binaries

| Binary | Description |
|--------|-------------|
| `tcode` | Per-project Claude-Code-compatible MPM orchestration harness |

## Subcommands (Phase 0 surface — stubs)

| Subcommand | Description |
|------------|-------------|
| `tcode serve --project <PATH>` | Start the per-project orchestration server (Phase 1) |
| `tcode run-task <agent> <task>` | Delegate a single task to a named agent (Phase 2) |
| `tcode run-workflow <name>` | Execute a named MPM workflow end-to-end (Phase 2) |

## Build

```bash
cargo build -p trusty-code
cargo run -p trusty-code -- --version
cargo test -p trusty-code
```

## Design Constraints

- **Claude-Code compatible** — reads `.claude/` config, agents, skills, MCP
  descriptors, `CLAUDE.md`, and permission grants exactly as Claude Code does.
- **Per-agent model routing** — each agent may specify its own model
  (AWS Bedrock or OpenRouter).
- **Single-instance per project** — one `tcode serve` process per `.claude/`
  root.
- **Event-driven** — will publish `HarnessEvent` via `trusty-common::events`
  when Phase 1 lands.

## Architecture Role

trusty-code is the bottom layer of the three-harness stack:

```
trusty-agents (general agentic)  →  delegates coding tasks to trusty-code
trusty-mpm (meta-harness)        →  launches and oversees trusty-code sessions
trusty-code (coding harness)     →  executes per-project coding workflows
```

See [docs/architecture/harnesses.md](../../docs/architecture/harnesses.md) and
[ADR-0004](../../docs/adr/0004-three-harnesses-shared-event-driven-common.md).
