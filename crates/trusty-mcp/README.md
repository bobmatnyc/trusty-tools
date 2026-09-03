# trusty-mcp

Shared JSON-RPC 2.0 / MCP protocol primitives for the trusty-* ecosystem.

Every trusty-* MCP server speaks the same line-delimited JSON-RPC over stdio and
advertises the same `rpc.discover` document. This crate holds that one
implementation so a parse-error fix or a `serverInfo` change lands once.

## What's here

| Module | Surface |
|---|---|
| (crate root) | `Request`, `Response`, `JsonRpcError`, `error_codes`, `initialize_response`, `run_stdio_loop` |
| `service` | `ServiceDescriptor` — the registration contract a linked service implements to contribute tools to a merged OpenRPC document |
| `openrpc` | `OpenRpcBuilder`, `discover_response` — build the `rpc.discover` manifest |
| `daemon_bridge` | `DaemonBridgeConfig`, `ensure_daemon_up` — probe/spawn/poll a service's HTTP daemon before entering the dispatch loop |
| `single_flight` | `StartLock`, `ensure_daemon_up_single_flight` — `flock(2)`-guarded variant so N concurrent bridge processes start exactly one daemon (#1152) |
| `daemon_bridge_json_rpc` | `UdsBridgeConfig`, `DaemonBridgeJsonRpc`, `run_stdio_bridge` — the shared stdio↔UDS JSON-RPC forwarder (#6316). **Feature-gated**, see below |
| `bin/trusty-mcp` | `trusty-mcp <service>` — the one stdio MCP bridge binary, driving the forwarder against any trusty daemon's socket (#6316). **Feature-gated**, see below |

## Features

| Feature | Default | What it turns on |
|---|---|---|
| `daemon-bridge-json-rpc` | off | The `daemon_bridge_json_rpc` module, and with it the crate's only dependency on `trusty-common` (its `uds` feature) |

Everything else in this crate is pure `serde` / `tokio` / `reqwest`. The
forwarder needs `trusty_common::uds`'s framed client, and that is a much heavier
edge than a crate every MCP server links should carry unconditionally — so it is
opt-in and the default rlib stays as lean as ADR-0040 left it. Prove the edge is
gated:

```bash
cargo tree -p trusty-mcp -e features -i trusty-common
# error: package ID specification `trusty-common` did not match any packages
cargo tree -p trusty-mcp -e features -i trusty-common --features daemon-bridge-json-rpc
# trusty-common v0.47.2 … └── trusty-mcp feature "daemon-bridge-json-rpc"
```

The feature also builds the `trusty-mcp` binary below — `required-features` on
the `[[bin]]`, so a default-featured `cargo check` simply has no such target.

## The `trusty-mcp <service>` binary

One stdio MCP bridge for every UDS-backed trusty daemon, per the 2026-07-24
"no per-crate MCP binaries" directive (#6316). An MCP client config names this
binary and a service instead of a different binary and a different verb per
daemon.

```bash
cargo install --path crates/trusty-mcp --features daemon-bridge-json-rpc --locked
trusty-mcp memory          # or trusty-mcp trusty-memory
trusty-mcp search
trusty-mcp analyze
```

It resolves the socket with `trusty_common::daemon_socket_path(<app>)` — the
same call the daemon makes to decide where to bind — and runs
`run_stdio_bridge`. Two things it deliberately does not do:

- **It starts nothing.** No probe, no spawn, no lock; a daemon's readiness guard
  stays that daemon's own (#1152 is the record of what N independently-spawning
  bridges cost). A request that arrives with nothing listening comes back as a
  JSON-RPC error carrying the request's own id — never silence, because an
  unmatchable answer is a hang to the client (#6309).
- **It writes nothing to stdout but JSON-RPC.** The usage text, the startup line
  and every failure go to stderr, a `--help` included.

| Exit | Meaning |
|---|---|
| 0 | stdin reached EOF, which is how an MCP client says stop (#457) — or `--help` |
| 1 | the socket path could not be resolved, or stdin/stdout failed |
| 2 | usage error: no service, an unknown service, or an extra argument |

The per-service streaming-method lists and frame budgets in `SERVICES` are a
second copy of each daemon's own constants — importing them would pull
trusty-memory, trusty-search and trusty-analyze into this crate's build for
three arrays and three integers. `the_table_matches_each_daemons_own_constants`
reads those crates' sources and fails on drift, which is the case #6286 showed
goes silent otherwise.

Two names sit close together and are not the same thing. `daemon_bridge` probes
and spawns a service's **HTTP** daemon before the dispatch loop starts.
`daemon_bridge_json_rpc` is the loop itself, forwarding each request to a daemon
over a **Unix socket**. A consumer can want either without the other.

The forwarder could not live here before [PR #6726](https://github.com/bobmatnyc/trusty-tools/pull/6726):
`trusty-common`'s `tickets` feature depended on `trusty-mcp`, so this edge would
have closed a cycle (#6316).

## History

These types lived in `trusty_common::mcp` (and, before that, in a standalone
`trusty-mcp-core`) until [ADR-0040](../../docs/adr/0040-trusty-mcp-services-absorbs-trusty-gworkspace.md)
extracted them again. `memory_rpc` did not come along: it resolves the
trusty-memory daemon's address through `trusty_common::daemon_addr`, so it
stays in `trusty-common` as `trusty_common::memory_rpc`.

There is deliberately no re-export shim in `trusty-common` —
`trusty_common::mcp` no longer exists.

## Testing

```bash
cargo test -p trusty-mcp --all-features --no-fail-fast
```

`--all-features` matters here: a bare run compiles `daemon_bridge_json_rpc` out
and its tests never execute (#4901's family).
