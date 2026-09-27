# Changelog

All notable changes to `trusty-mcp` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/); this crate uses
independent semantic versioning per the workspace convention.

This crate has not cut a release yet — everything written so far is pending in
`changelog.d/` and lands in the first released section (issue #4476).

---

## [0.2.0] — 2026-09-23

### Added

- `DaemonBridgeJsonRpc::with_local_handler` answers chosen MCP methods in the bridge process instead of forwarding them, so a daemon that is down at handshake no longer makes an MCP client mark the server failed for the whole session (#8351).
- `DaemonBridgeJsonRpc::with_socket_resolver` re-resolves the daemon's socket for every forwarded request, so a bridge that resolved a stale or wrong path heals on the next call rather than staying broken for the life of the process. A resolver that fails is reported as an error carrying the request's id, never downgraded to the configured path (#8351).
- `UdsBridgeConfig::with_bridge_version` names the consumer's build in transport-error text, so the next report of an unreachable daemon is attributable to a binary (#8351).

### Fixed

- The stdio bridge's first forwarded request dials under the longer startup retry bound, so a bridge launched before its daemon has bound the socket reaches it once the daemon comes up instead of failing for the whole session. Later requests keep the fast per-request bound (#8267).

### Changed

- The unreachable-daemon error names the bridge's build and says the next request is dialled fresh, so an operator can tell a transient outage from a wedged bridge and can attribute the report to a binary (#8351).

## [0.1.5] — 2026-09-13

### Added

- `config`, a default-off feature carrying the one MCP-server configuration authority the trusty-* crates share: `McpServerConfig` (name, `McpTransport::Stdio`/`Http`/`Sse`, an enable flag, and an uninterpreted `extensions` map for consumer-specific keys), `McpConfigFile::{load, load_or_default, save}` over `~/.trusty-tools/mcp/servers.toml` with an atomic write, `resolve` for layering per-consumer overrides on a global list, and `config::claude_code::{read_mcp_servers, write_mcp_servers}` as pure conversions to and from the `mcpServers` map Claude Code reads. The three transports match `trusty_mpm::core::mcp_config::McpTransport` one for one, so routing `tm mcp add` through the adapter loses none of them. A server's `env` and `headers` hold credentials, so on unix `save` writes the file `0600` rather than at the default umask, and `McpTransport`'s hand-written `Debug` prints those maps' key names with every value replaced by `<redacted>`. TOML has no null, so `save` drops a null `extensions` member — the shape a consumer's `Option::None` field takes — rather than failing the whole file with `toml`'s unnamed `unsupported unit type`, and refuses a null INSIDE an array with `McpConfigError::NullExtensionValue` naming the server and the dotted key, because dropping an element would renumber the rest (#7452).
- `McpConfigFile::parse` and `McpConfigFile::render` expose the parse and render halves of `load`/`save`, so a consumer doing a locked read-modify-write can decide from the bytes it already read instead of reading the path a second time outside its lock (#7454). `load` and `save` are unchanged and still the right entry points for a plain read or write.

## [0.1.4] — 2026-09-03

### Added

- **New crate: the JSON-RPC 2.0 / MCP protocol primitives every trusty-* MCP server shares** — `Request`, `Response`, `JsonRpcError`, `error_codes`, `initialize_response`, `run_stdio_loop`, `ServiceDescriptor`, `OpenRpcBuilder`/`discover_response`, `DaemonBridgeConfig`/`ensure_daemon_up`, and `StartLock`/`ensure_daemon_up_single_flight`. These moved verbatim out of `trusty_common::mcp`; the code is unchanged apart from import paths, so no behaviour differs from trusty-common 0.35.1 ([#5803](https://github.com/bobmatnyc/trusty-tools/issues/5803), ADR-0040)
  - `single_flight` moved with `daemon_bridge` even though it post-dates ADR-0040's extraction table — it landed in #5750 and has zero non-`mcp` coupling
  - `memory_rpc` deliberately stayed in `trusty-common`: it resolves the trusty-memory daemon through `trusty_common::daemon_addr` and `trusty_common::data_dir`
- **`trusty-mcp <service>` — one stdio MCP bridge binary for every trusty daemon.** `trusty-mcp memory`, `trusty-mcp search` and `trusty-mcp analyze` (each also spelled in full, `trusty-mcp trusty-memory`) read line-delimited JSON-RPC on stdin and forward it to that daemon's Unix socket, resolving the socket through `trusty_common::daemon_socket_path` — the same call the daemon makes to decide where to bind. This is the 2026-07-24 "no per-crate MCP binaries" directive: an MCP client config names one binary and a service rather than a different binary and a different verb per daemon ([#6316](https://github.com/bobmatnyc/trusty-tools/issues/6316))
  - It starts nothing. A daemon's readiness guard stays that daemon's own, and a request arriving with nothing listening is answered with a JSON-RPC error carrying the request's id rather than silence (#6309, #1152)
  - Stdout carries JSON-RPC frames and nothing else — the usage text, the startup line and every failure go to stderr, a `--help` included. An unknown service exits 2 with an empty stdout
  - Behind `required-features = ["daemon-bridge-json-rpc"]`, so `cargo check -p trusty-mcp` and `cargo check --workspace` stay free of `trusty-common`
  - Per-service streaming-method lists and frame budgets are a second copy of each daemon's own constants, because importing them would pull trusty-memory, trusty-search and trusty-analyze into this crate's build. `the_table_matches_each_daemons_own_constants` reads those crates' sources and fails on drift — the case #6286 showed goes silent otherwise
- **`daemon_bridge_json_rpc` — one shared stdio↔UDS JSON-RPC forwarder.** `UdsBridgeConfig`, `DaemonBridgeJsonRpc` and `run_stdio_bridge` read line-delimited JSON-RPC from stdin, forward each request to a daemon over its Unix socket with `trusty_common::uds::send_framed_request_capped`, and write the daemon's answer back. trusty-memory and trusty-analyze each carried their own copy of this; two copies drift, and #6286 showed how quietly — a streaming method added to the daemon and not to the bridge's refusal list left an MCP client waiting for a frame that was never coming. The streaming-method list, the request timeout, the frame budget and the daemon's name in error text are all the caller's; `with_request_rewriter` is where a consumer injects what it needs into each envelope (trusty-memory's `--palace` default and caller identity). trusty-memory and trusty-analyze are re-pointed onto it separately ([#6316](https://github.com/bobmatnyc/trusty-tools/issues/6316))
  - A daemon that is not listening, one that never answers, and one that answers with something that is not a JSON-RPC response each produce a JSON-RPC error naming the cause and carrying the request's own id. None produce an empty result and none end the loop — an unmatchable answer is a hang to the client (#6309)
  - `jsonrpc` is stamped to `"2.0"` on every forwarded envelope, because `RpcRouter` refuses anything else and `Request` serialises an omitted field as `null` (#6286). A request rewriter cannot undo it: the field is re-stamped after the rewrite runs
  - Behind the new `daemon-bridge-json-rpc` feature, off by default. It is the crate's only `trusty-common` edge, and a consumer that wants just the protocol primitives should not pay for it. The edge became possible at all when [#6726](https://github.com/bobmatnyc/trusty-tools/pull/6726) removed `trusty-common`'s dependency on this crate

### Fixed

- **The crate-level doc no longer links a feature-gated module.** `lib.rs` described `daemon_bridge_json_rpc` with a `[bracketed]` intra-doc link, but that module is compiled only under `daemon-bridge-json-rpc` and this crate's default feature set is empty. `scripts/check_rustdoc_links.sh` runs `cargo doc` with DEFAULT features, so the link had no item to resolve against and `#![deny(rustdoc::broken_intra_doc_links)]` turned it into a hard error — the gate reported `trusty-mcp has 1 broken link(s) and no baseline row`. The module is now named in a plain code span, which reads the same with the feature on or off ([#6316](https://github.com/bobmatnyc/trusty-tools/issues/6316))

### Changed

- Version skipped from 0.1.1 to 0.1.2 with no code change. The
  `trusty-mcp-v0.1.1` tag is pinned to a commit that fails the pre-publish
  gate, and #6178 makes a release tag here immovable, so 0.1.1 is spent.
- `daemon_bridge`'s docs no longer name trusty-memory as a caller. Its stdio bridge polls its own Unix socket in a local readiness loop since #6286 — the same disposition trusty-analyze took — so trusty-search is the module's one remaining consumer. No code change
- `daemon_bridge`'s trusty-analyze row describes a Unix socket rather than an
  HTTP endpoint on port 7879 (#6287, ADR-0032).

