# CLI Reference

`ta` (trusty-assistants) carries four verbs. #11632 cut the PM orchestrator,
the REPL, the workflow, direct and sub-agent modes, the `--api`/`--serve` HTTP
server and every subcommand that drove them; passing one of their flags is a
usage error.

```
ta --version             Print the version and the commit that built it
ta --help                Print the usage text
ta config <verb> …       Manage inference provider credentials (API keys)
ta config profile <verb> Show or set the user profile
ta mcp-serve             Serve the dispatch_task tool as a stdio MCP server
```

## `ta mcp-serve`

A stdio MCP server (JSON-RPC 2.0, protocol `2024-11-05`). It answers
`initialize`, `tools/list`, `tools/call` and `ping`, and exposes one tool,
`dispatch_task`, which hands a self-contained task to the opaque PM bridge
rooted at the server's working directory. It opens no network listener.

```bash
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize"}' | ta mcp-serve
```

## `ta config`

The shared credential verbs every trusty-* binary mounts
(`ta config keys list`, `set`, `delete`, …), plus
`ta config profile show|set --name … --email … --timezone … --location …`
for `~/.trusty-agents/user.toml`.

---

[← Back to user docs](./README.md)
