# trusty-agents

The assistant core of the trusty-* tools: per-assistant homes and OKG stores,
channels and listeners, memory and knowledge, plus one serving surface — a
stdio MCP server.

#11632 cut the agent harness this crate used to be: the PM orchestrator and
CTRL controller, sub-agent subprocesses and their NDJSON IPC, the REPL, the
workflow engine, the tmux manager, the HTTP API (`tagent --api`, `--serve`,
`--port`), its Svelte/Tauri desktop UI, the Slack and Telegram bot gateways,
and the coding tools (shell, git, filesystem, AST, ticketing). Coding work
belongs to `trusty-code`; multi-project orchestration belongs to `trusty-mpm`.

## The binary

```
tagent --version             Print the version and the commit that built it
tagent --help                Print the usage text
tagent config <verb> …       Manage inference provider credentials (API keys)
tagent config profile <verb> Show or set the user profile
tagent mcp-serve             Serve the dispatch_task tool as a stdio MCP server
```

`tagent` opens no network listener. `tests/no_network_listener.rs` fails on any
non-test TCP or UDP bind in `src/`.

## The library

| Module | What it holds |
|---|---|
| `assistants` | Assistant homes, instance ids, roster, provisioning, memory grants and policy |
| `agents` | The assistant manifest (`AgentConfig`) and its loaders |
| `channels`, `listeners`, `operations` | Channel bindings, sends and reads; listener config, event store and filters |
| `kb`, `knowledge`, `stores` | The OKG tree over `trusty-okf`, knowledge pipeline state, store bindings |
| `memory`, `tools` | trusty-memory recall, OKG and search tools, `dispatch_task` (`tools::pm_bridge`), `delegate_to_agent` |
| `intent` | The deterministic tm/tcode routing for `dispatch_task` |
| `mcp`, `plugins`, `rbac`, `rpc`, `usage`, `identity`, `untrusted` | MCP config, plugins, access tiers, OpenRPC discovery, usage ledger, user profile, untrusted-content fencing |

## Build and test

```bash
cargo check -p trusty-agents
cargo test -p trusty-agents --no-fail-fast
cargo clippy -p trusty-agents --all-targets -- -D warnings
```

## License

MIT.
