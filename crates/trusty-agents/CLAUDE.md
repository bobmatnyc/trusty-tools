# trusty-agents

The assistant core (library) and the `tagent` binary. Workspace rules live in
the repo-root `CLAUDE.md`; this file adds only what is specific to this crate.

## Scope after #11632

The agent harness was cut: no PM orchestrator, CTRL, REPL, workflow engine,
sub-agent subprocesses, tmux, HTTP API, desktop UI, or Slack/Telegram bot
gateways. Do not reintroduce them here. Coding work belongs to `trusty-code`;
multi-project orchestration belongs to `trusty-mpm`.

- The only serving surface is `tagent mcp-serve` (stdio MCP, `src/mcp_serve.rs`).
- No network listener outside `#[cfg(test)]` code. `tests/no_network_listener.rs`
  enforces it; do not widen its exclusions.
- `src/operations/` holds the assistant channel, listener and knowledge
  operations that used to sit under the HTTP API. Their error shape,
  `(StatusCode, Json<Value>)`, is kept so callers and tests did not change.
- Some inbound and write paths in `channels`, `listeners` and `operations` lost
  their callers with the cut and carry `#[allow(dead_code)]` with a `#11632`
  pointer. They are kept for the inbound rebuild, not for new callers to grow.

## Commands

```bash
cargo check -p trusty-agents
cargo test -p trusty-agents --no-fail-fast
cargo clippy -p trusty-agents --all-targets -- -D warnings
```
