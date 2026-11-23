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
ta list                  List the assistant instances `ask` accepts
ta ask <instance> <question…>
                         Ask an instance; prints the answer on stdout
ta handoff <instance> --to tm|tc [--key <key>] [--] <task…>
                         Hand a task to trusty-mpm or trusty-code
ta config <verb> …       Manage inference provider credentials (API keys)
ta config profile <verb> Show or set the user profile
ta mcp-serve             Serve the dispatch_task tool as a stdio MCP server
```

`ta <verb> --help` (or `-h`) prints that verb's usage and exits 0.

`ta` opens no network listener. `tests/no_network_listener.rs` fails on any
non-test TCP or UDP bind in `src/`.

## First steps

An instance is a directory `~/trusty-agents/<id>/` that holds
`instructions.md`. `ta` does not create instances. A machine where the older
`tagent` ran already has homes for its shipped assistants, such as `izzie` and
`assistant`.

1. List the instances:

   ```bash
   ta list
   ```

   With none, `ta list` prints one line naming the directory it searched.
2. If the list is empty, create one. The id is lowercase letters, digits,
   `-`, `_` or `.`:

   ```bash
   mkdir -p ~/trusty-agents/izzie
   printf 'You are izzie, a personal assistant.\n' > ~/trusty-agents/izzie/instructions.md
   ```

3. Ask it a question. The words after the instance are one question:

   ```bash
   ta ask izzie "what is on my list today?"
   ```

The model call needs an inference provider key; `ta config keys --help` shows
how to set one. An unknown instance is an error that names the instances that
exist.

## `ta ask`

`ta ask <instance> <question…>` prints the instance's answer on stdout and
nothing else, so a script can capture it. The first ask starts the instance's
daemon (`ta ask-daemon <instance>`), which takes about 10 s; during that start
`ta` prints one line on stderr, `ta: starting <instance> assistant…`. Later
asks reuse the running daemon and print no progress line. The daemon exits
when idle. Each turn is appended to `~/trusty-agents/<id>/stores/ask-turns.jsonl`.

## `ta handoff`

`ta handoff <instance> --to tm|tc [--key <key>] [--] <task…>` hands one task
from an instance to trusty-mpm (`tm`) or trusty-code (`tc`) and prints one JSON
line with the key, the target and the target's handle.

| Flag | Meaning |
|---|---|
| `--to tm\|tc` | Required; there is no default. `tm` sends into the instance's durable `tm serve --stdio` session, created on the first handoff. `tc` runs `task.run` on the shared tcode socket. |
| `--key <key>` | Idempotency key. The same key again replays the first handoff instead of sending twice. Omitted, a new key is minted and printed. |
| `--` | End of options; every later word is task text. |

Both targets run in the `project_dir` that the instance's `config.toml` names;
without one, `tm` uses the instance home.

```bash
ta handoff izzie --to tc -- "add a --dry-run flag to the export script"
```

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
exited idle. `--role` writes the instance's `instructions.md` from the
personal or CTO template. Offline, for both instances:

```bash
TCODE_MOCK_LLM=echo scripts/sandbox_assistants_ask.sh --dir "$D" --role personal izzie "hello"
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
