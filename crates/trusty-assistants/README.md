# trusty-assistants

Formerly `trusty-agents`; #11632 renamed the crate and its binary (`tagent`
is now `ta`, and `tagent` stays as a deprecated alias for one release).

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
ta --version             Print the version and the commit that built it
ta --help                Print the usage text
ta config <verb> …       Manage inference provider credentials (API keys)
ta config profile <verb> Show or set the user profile
ta mcp-serve             Serve the dispatch_task tool as a stdio MCP server
```

`ta` opens no network listener. `tests/no_network_listener.rs` fails on any
non-test TCP or UDP bind in `src/`.

## Where it keeps data

The rename did not move any data (#11632, owner ruling Dcm):

| What | Path |
|---|---|
| Assistant homes | `~/trusty-agents/<id>/`, or `$TA_ASSISTANTS_DIR`, then `$TAGENT_ASSISTANTS_DIR` |
| Config and per-user state | `~/.trusty-agents/` |

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

## Install

Install `ta` only, with `scripts/install-trusty-assistants-signed.sh` (#11650).
It runs `cargo install ... --bin ta --locked`, signs `ta` alone, and exits 1 if
the `tagent` in the install root changed. Never install the `tagent` alias:
the `tagent` in `~/.cargo/bin` comes from the older `trusty-agents` package and
runs a launchd Slack bot. Sources and signing:
[release-workflow.md](../../docs/reference/release-workflow.md#signed-install-trusty-assistants-4277).

## Sandbox smoke run

`scripts/sandbox_assistants_ask.sh` runs one `ta ask` under `env -i` with a
temporary `HOME` and data directory, then checks that the instance's daemon
exited idle. Offline, for both instances:

```bash
TCODE_MOCK_LLM=echo scripts/sandbox_assistants_ask.sh --dir "$D" --role personal personal "hello"
TCODE_MOCK_LLM=echo scripts/sandbox_assistants_ask.sh --dir "$D" --role cto cto-assistant "hello"
```

## Build and test

```bash
cargo check -p trusty-assistants
cargo test -p trusty-assistants --no-fail-fast
cargo clippy -p trusty-assistants --all-targets -- -D warnings
```

## License

MIT.
