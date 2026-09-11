# trusty-agents 1.0 item (g): MCP connectors per-assistant and global — gap analysis (2026-09-11)

Owner requirement: MCP connectors configurable per assistant as well as
globally, with `trusty-mcp` owning MCP connection/config code, shared between
`trusty-code` and `trusty-agents` via one config file; `trusty-mpm` keeps using
Claude Code's own MCP config. Item (g) is not yet in epic #7425's body (which
lists only a–f) — this is new scope stated in this session.

## 1. `crates/trusty-mcp` — what exists today

`Cargo.toml:2-35` — package `trusty-mcp`, v0.1.4, `rlib` + one `required-features`-gated
bin (`trusty-mcp`, `src/bin/trusty-mcp.rs`). Consumers (`grep -rn trusty-mcp --include=Cargo.toml crates/`):
`trusty-agents`, `trusty-memory`, `trusty-channels`, `trusty-gworkspace`,
`trusty-kb`, `trusty-analyze`, `trusty-review` (optional), `trusty-code`,
`trusty-mpm`, `trusty-search`. All depend on it for the same reason: shared
JSON-RPC/MCP wire types.

What it contains, by file:
- `src/lib.rs:1-70` — `Request`/`Response`/`JsonRpcError`, `error_codes`,
  `initialize_response`, `run_stdio_loop` (the shared stdio dispatch loop every
  native trusty-* MCP **server** uses).
- `src/daemon_bridge.rs:1-60+` — `DaemonBridgeConfig` / `ensure_daemon_up`: HTTP
  health-probe-and-autostart guard for a stdio bridge in front of an HTTP daemon.
- `src/daemon_bridge_json_rpc.rs` (feature `daemon-bridge-json-rpc`) —
  `DaemonBridgeJsonRpc` / `run_stdio_bridge`: the stdio↔UDS forwarder.
- `src/bin/trusty-mcp.rs:1-40+` — the `trusty-mcp <service>` binary (#6316):
  one stdio↔UDS bridge process for trusty-memory/trusty-search/trusty-mpm.
  "It does not start anything" (line 18) — no spawn, no config, no registry.
- `src/openrpc.rs` — `OpenRpcBuilder`, `discover_response`: builds the
  `rpc.discover` manifest document.
- `src/service.rs:1-44` — `ServiceDescriptor` trait (name/version/tools/scopes)
  for a host process to aggregate several services into one OpenRPC manifest.
- `src/single_flight.rs` — `StartLock` / `ensure_daemon_up_single_flight`
  (flock-based crash-safe start guard).

**It does NOT contain**: any `McpServerConfig` type, any on-disk config file
format, any loader, or any notion of "a list of MCP servers I should connect
to" (client-side registry). It is entirely SERVER-side (how a trusty-* daemon
exposes itself as MCP) plus one CLIENT-transport primitive (the stdio↔UDS
forwarder, which only ever dials trusty's own three UDS daemons — the table in
`bin/trusty-mcp.rs` is hardcoded to memory/search/mpm, not configurable).

This matches the ADR history: ADR-0033 (`trusty-mcp consolidates native MCP
services into one crate` — gworkspace/channels/kb as library modules) was
**superseded by ADR-0040**, which extracted only the JSON-RPC/MCP *protocol*
primitives from `trusty-common` into `trusty-mcp`, explicitly not the
gworkspace/channels/kb hosting vision. Epic **#5066** ("epic: trusty-mcp unified
MCP adapter crate") is still OPEN and describes the wider vision as unfinished
work — `trusty-mcp` today is a lean protocol crate, not the adapter-of-adapters
epic #5066 describes.

## 2. trusty-mpm — `tm mcp add/list/remove`

`crates/trusty-mpm/src/bin/tm/commands/mcp.rs:1-13` — thin CLI shell;
`crates/trusty-mpm/src/core/mcp_config.rs:1-27` is the CRUD implementation.
It writes the **user-scope `mcpServers` map of `<CLAUDE_CONFIG_DIR>/.claude.json`**
— Claude Code's own format, not `.mcp.json` (that's the separate PROJECT-scope
file, `MCP_JSON` const at `mcp_config.rs:39`, read elsewhere for provenance/doctor
checks, not written by `tm mcp add`).

- Types: `McpTransport` enum (`Stdio`/`Http`/`Sse`, `mcp_config.rs:183`),
  `build_stdio_entry`/`build_remote_entry` (`mcp_config.rs:242,263`) producing
  raw `serde_json::Value` in Claude Code's on-disk shape
  (`{"type","command","args","env"}` / `{"type","url","headers"}`).
- This is **entirely trusty-mpm's own code** — it does not import `trusty-mcp`
  for any of it (trusty-mpm's `trusty-mcp` dependency, `Cargo.toml:204`, is used
  elsewhere for the stdio↔UDS bridge, not for this CRUD).
- Separately, `trusty_common::claude_config::mcp_server_entry`
  (`crates/trusty-common/src/claude_config.rs:150-155`) builds a THIRD,
  minimal `{command,args}` entry shape for GUI-client registration
  (`crates/trusty-common/src/gui_mcp_client.rs:1-25`) — a third independent
  "build an MCP server config value" implementation.
- No loader/registry type is shared with `trusty-mcp` or `trusty-common`; this
  confirms the owner's framing that trusty-mpm keeps using Claude Code's own
  format and stays outside whatever `trusty-mcp` grows into.

## 3. trusty-code — MCP configuration for its harness

trusty-code has **no persistent MCP server config file or registry of its
own today**. What it has:
- `Cargo.toml:105-131` — depends on `trusty-mcp` (wire types only, "JSON-RPC /
  MCP primitives") and on `trusty-common`'s `stdio-mcp-client` feature
  ("provides the shared `StdioMcpClient`... the trusty-search-backed
  code-discovery tool reuses instead of writing its own MCP transport").
- `src/skills/protocol.rs:547` — uses `trusty_mcp::Request` (wire types only).
- `src/plugins/mod.rs:5,91` — plugin manifests carry `mcpServers`/`mcp` keys
  in `LATER_PHASE_MANIFEST_KEYS` (`mod.rs:91`) explicitly marked **not yet
  implemented** ("later phases").
- Issue **#5428** "feat(trusty-code): load and execute shared MCP server
  definitions" (OPEN, milestone "trusty-code R3 · MCP & channel
  interoperability") is exactly this gap: define `.trusty-code/` ownership,
  validate/spawn/supervise stdio MCP servers from "portable server
  definitions accepted by the wider Trusty platform" — i.e. the shared file
  the owner is now asking for does not exist yet; #5428 is its tracking issue.
- **ADR-0058** (`trusty-code is an independent product-owned harness`,
  decision 5, `docs/adr/0058-…:76-84`): "Code imports versioned MCP server
  definitions and channel envelope schemas, records non-secret project
  references and provenance under `<project>/.trusty-code/`, resolves trusted
  executable definitions and mutable lifecycle state under `~/.trusty-code/`
  ... Code does not mutate MPM's managed configuration." This is the
  architectural commitment the owner's item (g) slots into: trusty-code
  CONSUMES a shared definition format but owns its own resolved/runtime state
  privately; it does not write into trusty-mpm's `.claude.json`.
- ADR-0058 amends **ADR-0042** ("MCP configuration is static and persistent")
  for the trusty-code boundary specifically — trusty-mpm's static user-scope
  declarations stay as they are (§4 above); trusty-code resolves its own
  imported declarations separately.

Conclusion: trusty-code's MCP story today is 100% "consume `trusty-mcp` wire
types + `trusty-common::StdioMcpClient` to talk to a server", zero "load a
persisted list of configured servers". The shared config file the owner wants
for trusty-code + trusty-agents does not exist in either crate yet.

## 4. trusty-agents — how an agent attaches MCP endpoints today

trusty-agents has **three independent MCP-server-shape schemas** plus a
fourth "serve" surface, all in one crate:

1. **`crate::mcp::config` — `[mcp]` section of `~/.trusty-agents/config.toml`**
   (`crates/trusty-agents/src/mcp/config/mod.rs:1-45`, `types.rs:132-186`):
   `McpSection { inject_for_roles, services: Vec<McpService>,
   trust_project_mcp_json }`; `McpService { name, description, command, args,
   env: HashMap<String,String>, url: Option<String>, transport: String,
   enabled, tools: Vec<McpTool>, discover }`. This is the **global-only**
   registry — `GlobalConfig` (`mod.rs:57-`) has no per-assistant variant.
2. **`crate::tools::registry::config` — `[[tool_registry.endpoints]]`**
   (`crates/trusty-agents/src/tools/registry/config.rs:18-100`):
   `ToolRegistryConfig { scope_enforcement, endpoints: Vec<EndpointConfig> }`;
   `EndpointConfig { name, driver: DriverKind(Direct|StdioMcp), description,
   url, command, args, enabled, scopes, discovery_ttl_secs, eager_discovery,
   auth: Option<AuthConfig>, transport: Option<TransportConfig> }`. This is
   the OpenRPC/`rpc.discover` path (spec DOC reference:
   `docs/specs/agent-config-five-sections.md` §4.4 K-c) — also global-only,
   also `~/.trusty-agents/config.toml`, a **different Rust type** for a
   structurally similar "name a command/URL, enable it, give it scopes"
   concept than `McpService` above.
3. **`crate::mcp::mcp_json` — `.mcp.json` parser**
   (`crates/trusty-agents/src/mcp/mcp_json.rs:1-18`): `McpJsonServer`,
   `discover_mcp_json_paths`, `parse_mcp_json_servers` — reads the SAME
   Claude-Code-format `.mcp.json` `mcpServers` map that trusty-mpm's
   `mcp_config.rs` writes, independently re-implemented (project file found
   by directory walk, then user-global `~/.claude/.mcp.json`). Feeds
   `tools::mcp_live` (live spawn+discover, gated by `McpSection.
   trust_project_mcp_json`) and `init::seed::mcp` (memory-seeding only).
4. **`crate::runtime::mcp_serve`** (`runtime/mcp_serve.rs:1-25`) — trusty-agents
   itself AS an MCP **server** via `trusty_mcp::run_stdio_loop` (reuses
   trusty-mcp correctly, this direction).

Per-assistant vs. global: **no per-assistant MCP override exists today.**
`~/.trusty-agents/config.toml`'s `GlobalConfig` is process-global. However,
the per-assistant HOME already exists structurally for exactly this purpose:
`crates/trusty-agents/src/assistants/home.rs` (#4325) defines
`AssistantHome` with five layout entries including `CONFIG_FILE = "config.toml"`
(`home.rs:80`) at `<assistants_root>/<instance>/config.toml`, and
`AssistantHomeConfig` (`home.rs:365-372`): today just `{ id, display_name }`,
explicitly documented as "unknown keys are IGNORED, not rejected" — i.e.
additive. **This is the natural per-assistant override location**: adding an
`[mcp]` (or `[[mcp.overrides]]`) table to `AssistantHomeConfig` is additive and
matches the existing home-health tolerance contract (`home.rs:355-364`).

Spec coverage: `docs/specs/agent-config-five-sections.md` §4.4 "K-c — MCP
knowledge connections" (lines 236-260) already frames MCP endpoints as a
first-class agent-config pane, with a documented "honest gap": trusty-memory
and trusty-search endpoints are declared as `[[tool_registry.endpoints]]`
`driver = "direct"` but `enabled = false` forever (the binaries never
implemented an OpenRPC `--rpc` mode) — superseded on `main` by live
`[[mcp.services]]` `discover = true` entries instead
(`assets/config/default-config.toml:153-184`). Only `gworkspace` is a live,
enabled `[[tool_registry.endpoints]]` entry today
(`default-config.toml:287-317`).

## 5. Duplication audit vs. the common-entry-point rule

| Capability | Independent implementations found |
|---|---|
| **Spawn + speak MCP over child stdio (client)** | `trusty_common::stdio_mcp_client::StdioMcpClient` (`crates/trusty-common/src/stdio_mcp_client/{mod,client}.rs`) — the ALREADY-CONSOLIDATED one. Originated in `trusty-agents/src/plugins/stdio_mcp`, promoted to `trusty-common` in epic #1104 Phase 0a specifically so trusty-console and others avoid a `trusty-agents` dependency (`Cargo.toml:854-861`, feature `stdio-mcp-client`). `trusty-code` already consumes this one (§3). `trusty-agents`'s own `tools::mcp_live::executor` and `runtime::tool_registry`'s `driver.rs`/`direct.rs` implement their OWN spawn+JSON-RPC-over-stdio logic for the `[[mcp.services]]`/`[[tool_registry.endpoints]]` paths rather than reusing `StdioMcpClient` (unverified without reading `driver.rs`/`executor.rs` bodies in full — flag for follow-up, but the doc comments at `stdio_mcp_client/mod.rs:1-10` name only trusty-console as a non-agents consumer, so trusty-agents's own live-MCP paths likely predate or bypass the shared client). |
| **Build/serialize an MCP server config *entry*** | (a) `trusty_mpm::core::mcp_config::{build_stdio_entry, build_remote_entry}` — Claude Code `.claude.json`/`.mcp.json` shape. (b) `trusty_common::claude_config::mcp_server_entry` — a third, minimal `{command,args}` shape for GUI-client registration (`gui_mcp_client.rs`). (c) `trusty_agents::mcp::config::McpService` — TOML shape. (d) `trusty_agents::tools::registry::config::EndpointConfig` — a DIFFERENT TOML shape for structurally the same idea, in the same crate. **Four shapes, four crates/modules, zero sharing.** |
| **Read/parse a `.mcp.json` file** | `trusty_mpm::core::mcp_config` (writer + reader, Claude Code format) and `trusty_agents::mcp::mcp_json` (independent reader, same file format, own struct `McpJsonServer`). Two parsers of the identical on-disk format. |
| **MCP server as a "service registry" the harness advertises to an LLM** | `trusty_agents::mcp::config::McpSection` (services list) vs. `trusty_agents::tools::registry::config::ToolRegistryConfig` (endpoints list) — both are "list of remote tool sources with enable flags and scopes", coexisting in one crate for historical reasons (`[[mcp.services]]` grew as the live-discovery replacement after `[[tool_registry.endpoints]]`'s `driver="direct"` OpenRPC path turned out to be dead code for trusty-memory/trusty-search, per `default-config.toml:153-184`). |
| **Serve MCP (server-side loop)** | Correctly consolidated: every native trusty-* daemon (`trusty-memory`, `trusty-search`, `trusty-agents::runtime::mcp_serve`) uses `trusty_mcp::run_stdio_loop` / `initialize_response`. No duplication here. |
| **trusty-common's own `mcp` module** | Does not exist post-ADR-0040 (fully extracted to `trusty-mcp`). `trusty-common` today only has `stdio_mcp_client` (client) and `gui_mcp_client` (a GUI-specific config-entry builder, see above) — confirmed via `grep -rn "pub mod mcp" crates/trusty-common/src/lib.rs` (no match). |

**Which should become THE implementation:** `trusty-mcp` for the *shared
config type + loader + resolver* (net-new — it owns none of this today), and
`trusty_common::stdio_mcp_client::StdioMcpClient` for the *client transport*
(already correct and already shared — extend it, don't replace it). The four
config-entry-builder implementations and the two `.mcp.json` parsers should
collapse onto whatever `trusty-mcp` defines in response to this epic.

## 6. Existing issues/epics (searched 2026-09-11)

| # | Title | State | Milestone |
|---|---|---|---|
| **5066** | epic: trusty-mcp unified MCP adapter crate | OPEN | Backlog · mpm/core |
| 3828 | trusty-mcp unified CLI multiplexer: single entry point for all native MCP services | OPEN | Backlog · mcp |
| **5428** | feat(trusty-code): load and execute shared MCP server definitions | OPEN | trusty-code R3 · MCP & channel interoperability |
| 4568 | feat(trusty-agents): replace inline McpService.env secrets with credential references | OPEN | Backlog · agents |
| 4315 | feat(trusty-agents): per-connection health/probe API for memory, search, and OKG | OPEN | Backlog · agents |
| 7422 | tm session launch: scope plugins and MCP servers to the project | OPEN | Backlog · mpm/core |
| 7276 | audit: Claude Code ↔ trusty-mpm config overlap — 10 surfaces | OPEN | Backlog · mpm/core |
| 7427 | feat(trusty-agents): two-way channel connectors (gworkspace/Slack/Telegram/Notion) | OPEN | trusty-agents 1.0 · assistant platform (sibling of item g, epic #7425) |
| 7425 | epic(trusty-agents): 1.0 assistant platform | OPEN | body lists (a)-(f) only — **(g) is not yet recorded here** |
| 3787 | trusty-agents: live-MCP tool registry rebuilds/re-spawns every chat turn — no cross-turn cache | CLOSED | PAUSED · agents |
| 3209 | chore(trusty-agents): migrate to shared credential resolver before wiring tm/tcode MCP tools | CLOSED | — |

No open issue anywhere asks for "per-assistant MCP config" specifically — the
closest is #4315 (per-connection health/probe) and #5428 (trusty-code side of
shared definitions). **#5066 is the right epic to own the `trusty-mcp` side of
this work**; it predates and generalizes item (g) but has gone stale relative
to what ADR-0040 actually shipped (protocol primitives only, not the
gworkspace/channels/kb hosting it originally described — that hosting
ambition was separately superseded by ADR-0040's narrower scope).

`docs/reference/crate-map.md:43` lists `trusty-mcp` as a `library` crate.
Specs/ADRs naming it: `docs/specs/README.md`, `docs/specs/DOC-47-external-event-ingestion.md`,
`docs/specs/SPEC-MCPSVC-01-trusty-mcp-service.md`, `docs/adr/0014`, `0033`
(superseded), `0034`, `0040`, `0041`, `0042` (amended by 0058), `0058`.

## 7. Proposed target shape

Add to `trusty-mcp` (net-new, behind a new default-off feature, e.g.
`config`, to keep the lean-rlib property ADR-0040 protects):

- `McpServerConfig { name, transport: McpTransport(Stdio|Http|Sse), command,
  args, env, url, headers, enabled, scopes }` — one shape covering every
  existing consumer's fields (superset of trusty-mpm's `McpTransport` +
  trusty-agents's `McpService`/`EndpointConfig` + `AuthConfig`).
- `McpConfigFile { servers: Vec<McpServerConfig> }` with `load`/`save`
  (TOML, matching trusty-agents's and trusty-code's existing convention) at a
  new shared path — recommend `~/.trusty-tools/mcp/servers.toml` (parallel to
  the workspace's existing `~/.trusty-tools/<crate>/` convention, e.g.
  `~/.trusty-tools/trusty-mpm/claude-config/`) as the ONE file trusty-code and
  trusty-agents both read, per the owner's framing. Do not touch trusty-mpm's
  `.claude.json`/`.mcp.json` — a Claude-Code-format **adapter** module
  (`trusty_mcp::claude_code_format::{to_claude_json_entry, from_claude_json_entry}`)
  replaces `trusty_mpm::core::mcp_config::{build_stdio_entry,build_remote_entry}`
  and `trusty_common::claude_config::mcp_server_entry` as pure functions over
  `McpServerConfig`, called BY `tm mcp add`, not making trusty-mpm depend on
  the shared file's load/save path.
- `resolve(global: &McpConfigFile, assistant_overrides: Option<&McpConfigFile>) -> Vec<McpServerConfig>`
  — assistant entries win on name collision, matching the existing precedent
  in `trusty_agents::mcp::mcp_json` (project `.mcp.json` entries already win
  over user-global ones by the same rule).

Consumers to change:
- `trusty-mpm`: `tm mcp add/list/remove` keeps writing `.claude.json` but
  builds the JSON value through the new adapter instead of its own
  `build_stdio_entry`/`build_remote_entry` (mechanical, low risk).
- `trusty-code`: implements #5428 against `McpConfigFile`/`resolve` instead of
  inventing its own schema — this is the crate that currently has nothing, so
  it is pure addition, not a migration.
- `trusty-agents`: `mcp::config::McpSection` and
  `tools::registry::config::ToolRegistryConfig` both migrate onto
  `McpServerConfig` (a real, non-trivial merge — two schemas collapsing to
  one, with `driver`/`discover`/`auth`/`transport` knobs surviving as optional
  fields); assistant-level overrides load from `AssistantHomeConfig`'s new
  `[mcp]` table (additive to `home.rs`'s tolerant-unknown-keys contract) and
  are resolved through `trusty_mcp::resolve`.
- Client transport stays `trusty_common::stdio_mcp_client::StdioMcpClient`
  for actually spawning stdio servers — `trusty-mcp` owns config shape and
  resolution, not process spawning (keeps `trusty-mcp` a lean rlib and avoids
  a `trusty-mcp` → `trusty-common` dependency edge in the wrong direction,
  since today's edge is `trusty-common` optionally depending on nothing from
  `trusty-mcp`, and `trusty-mcp`'s only `trusty-common` edge is the existing
  `daemon-bridge-json-rpc` feature).

**Rung** (per `docs/reference/test-ladder-baseline.md`): this is a **rung 4**
cross-crate change (new public API in `trusty-mcp`, consumed by
`trusty-mpm`/`trusty-code`/`trusty-agents`) — `cargo check --workspace` plus
`cargo test -p <consumer> --no-fail-fast` for each of the three, once
implemented. Design/schema-collapse work inside `trusty-agents` (merging two
existing config types) is itself rung 3 within that crate.

## 8. Ownership note

- **This session's scope (agents crates):** `trusty-agents`,
  `trusty-agents-common`, `trusty-agents-local` — per epic #7425's own scope
  boundary ("changes land only in trusty-agents, trusty-agents-common,
  trusty-agents-local").
- **Owned by the other core session:** `trusty-mcp`, `trusty-code`,
  `trusty-common`, `trusty-mpm`. Any change to `McpServerConfig`/
  `McpConfigFile`/the adapter belongs there; this analysis is a handoff
  input, not agents-crate work. `trusty-gworkspace`, `trusty-channels`,
  `trusty-kb` are explicitly out of scope for epic #7425 too (consumed as
  dependencies only).
