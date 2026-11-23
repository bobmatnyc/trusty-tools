# Quickstart

`trusty-agents` (binary `tagent`) is not published to crates.io — build it
from the `trusty-tools` monorepo source.

## 1. Install

```bash
git clone https://github.com/bobmatnyc/trusty-tools.git
cd trusty-tools
cargo install --path crates/trusty-agents --locked
tagent --version
```

On macOS, `make install-agents-signed` installs and signs it so TCC grants
survive a rebuild.

## 2. Connect an MCP client

`tagent mcp-serve` is a stdio MCP server exposing `dispatch_task`. Register it
with an MCP client as a stdio server whose command is `tagent mcp-serve`.

## What changed

#11632 cut the agent harness this guide used to walk through: the REPL, the
workflow modes, the `--api` HTTP server and its desktop UI. The assistant core
(channels, listeners, memory, knowledge, OKG) remains as a library; see the
[CLI reference](./cli-reference.md).

---

[← Back to user docs](./README.md)
