# 0060. One MCP configuration authority in trusty-mcp; global and per-assistant tiers

- **Status:** Proposed
- **Date:** 2026-09-11
- **Scope:** crate `trusty-mcp` (new config authority); consumed by
  `trusty-agents` and `trusty-code`; `trusty-mpm` stays on Claude Code's own
  `mcpServers` config, through a `trusty-mcp` adapter
- **Reversibility Cost:** Medium — a shared file format and a resolver
  function, but no stored user data migrates today: `trusty-agents`'s three
  schemas and `trusty-mpm`'s Claude Code writer have not yet diverged into
  persisted config this ADR must convert. Reversing later means re-splitting
  the config surface and re-diverging the schemas this ADR collapses.
- **Decision Drivers:** the owner's 2026-09-11 ruling for epic #7425 item (g);
  the duplication findings in
  `docs/research/trusty-agents-mcp-connectors-gap-analysis-2026-09-11.md`
  (four independent MCP-server config-entry builders, two `.mcp.json`
  parsers, three per-agent-only MCP-server schemas inside `trusty-agents`
  alone); the CLAUDE.md "Common entry point, clean domain demarcation" rule.
- **Supersedes / Superseded by:** Amends ADR-0042 (MCP configuration is static
  and persistent) for the shared-file and per-assistant-override case; extends
  ADR-0040 (trusty-mcp holds protocol primitives) into config ownership;
  consistent with ADR-0058 decision 5 (trusty-code imports versioned MCP
  server definitions and resolves executable lifecycle privately).
  Supersedes nothing.

## Context

`crates/trusty-mcp` today holds only server-side JSON-RPC/MCP wire types and
one client-transport primitive (the stdio↔UDS bridge dialing trusty's own
three daemons). It contains no `McpServerConfig` type, no on-disk config
format, and no loader — nothing modeling "a list of MCP servers I should
connect to." This is the intended scope from ADR-0040, which extracted only
protocol primitives from `trusty-common`, not the adapter-of-adapters vision
epic #5066 still describes.

In the absence of that authority, four independent modules each grew their
own MCP-server config shape:

1. `trusty_mpm::core::mcp_config::{build_stdio_entry, build_remote_entry}` —
   Claude Code `.claude.json`/`.mcp.json` shape.
2. `trusty_common::claude_config::mcp_server_entry` — a third, minimal
   `{command, args}` shape for GUI-client registration.
3. `trusty_agents::mcp::config::McpService` — a TOML shape for
   `~/.trusty-agents/config.toml`'s `[[mcp.services]]`.
4. `trusty_agents::tools::registry::config::EndpointConfig` — a different
   TOML shape for structurally the same idea, in the same crate.

`trusty-agents` also runs two independent `.mcp.json` parsers
(`trusty_mpm::core::mcp_config` and `trusty_agents::mcp::mcp_json`) over the
identical on-disk file. `trusty-code` has no MCP config story at all yet
(#5428 tracks it) and would otherwise invent a fifth shape. No open issue
asked for per-assistant MCP configuration before this epic; the closest prior
art, `AssistantHomeConfig`
(`crates/trusty-agents/src/assistants/home.rs`), already documents unknown
keys as ignored rather than rejected, which is the additive contract a new
`[mcp]` table needs.

The owner's 2026-09-11 ruling (epic #7425 item (g), design review) settles
the target shape: MCP connection-management code moves to `trusty-mcp`;
`trusty-code` and `trusty-agents` share one config file; `trusty-mpm` keeps
using Claude Code's own MCP config through an adapter; for an Assistant, an
MCP connection is set at the global level or the assistant level. Full
evidence:
`docs/research/trusty-agents-mcp-connectors-gap-analysis-2026-09-11.md`.

## Decision

1. **`trusty-mcp` is the one authority for MCP connection/configuration
   code.** It gains this responsibility net-new, behind a default-off feature,
   so it stays the lean rlib ADR-0040 protects. No other crate defines its own
   MCP-server config type going forward.
2. **Global tier: one shared file.** `McpConfigFile` (`servers:
   Vec<McpServerConfig>`) loads and saves at one path shared by `trusty-code`
   and `trusty-agents` — `~/.trusty-tools/mcp/servers.toml`. `McpServerConfig`
   covers name, transport (`stdio` | `http`), command/args/env or url/headers,
   and `enabled`.
3. **`trusty-mpm` does not adopt the shared file.** It keeps writing Claude
   Code's own `mcpServers` map in `.claude.json`/`.mcp.json`, through a
   `trusty_mcp::claude_code_format` adapter — pure functions over
   `McpServerConfig` that replace `trusty-mpm`'s own
   `build_stdio_entry`/`build_remote_entry` and
   `trusty_common::claude_config::mcp_server_entry`. `trusty-mpm` does not
   depend on the shared file's load/save path.
4. **Assistant tier: additive override on `AssistantHomeConfig`.**
   `<assistant home>/config.toml` gains an `[mcp]` table, modeled as
   `trusty_mcp::config::McpServerOverride` entries, additive to
   `AssistantHomeConfig`'s existing tolerant-unknown-keys contract. An
   override adds a server, replaces one by name (wholesale — no field merge
   with the global entry), or disables one by name. An assistant may
   re-enable a server the global file disabled.
5. **One resolver.** `trusty_mcp::config::resolve(global, overrides) ->
   Vec<McpServerConfig>` computes the effective set for one assistant;
   assistant entries win by name.
6. **Failure handling is fail-closed per config, never fatal per server.** A
   malformed global file starts the assistant with zero MCP servers and a
   visible warning. A malformed assistant override falls back to the global
   set with a visible status. A server whose credential does not resolve is
   skipped with a per-server status; it never fails assistant startup.
7. **No credential-reference field ships with this decision.** `McpServerConfig`
   carries inline fields only, matching today's `McpService.env` shape;
   `#4568` owns the credential-reference design and this ADR does not
   pre-empt it.
8. **Client transport is unchanged.**
   `trusty_common::stdio_mcp_client::StdioMcpClient` stays the transport that
   actually spawns and speaks to stdio servers. `trusty-mcp` owns config shape
   and resolution, not process spawning.

## Consequences

- Four independent config-entry-builder implementations and two `.mcp.json`
  parsers collapse onto one type and one adapter. A future consumer (e.g. a
  fifth harness) extends `trusty-mcp` instead of inventing a fifth shape.
- `trusty-code` (#5428) gets a config format to consume against instead of
  inventing its own — pure addition for that crate, since it owns no MCP
  config today.
- `trusty-agents` must migrate two existing schemas
  (`mcp::config::McpSection`, `tools::registry::config::ToolRegistryConfig`)
  onto `McpServerConfig` — a non-trivial merge, tracked in #7454, since
  `driver`/`discover`/`auth`/`transport` knobs must survive as optional
  fields.
- Delivery is four separate PRs behind one shared shape, coordinated across
  two sessions: `trusty-mcp` (#7452), `trusty-mpm` (#7453), `trusty-code`
  (#5428), `trusty-agents` (#7454). #7452 blocks the other three.
- `trusty-mpm`'s user-visible behavior is unaffected — `tm mcp add/list/remove`
  keeps writing `.claude.json` in the same shape; a golden test on the written
  file is the proof obligation for #7453, not a behavior change.
- Per-assistant MCP configuration becomes possible for the first time; no
  prior decision defined this axis.

## Related Decisions

Vetted against `docs/adr/INDEX.md` on 2026-09-11:

- **ADR-0040 (Split MCP framework and services):** Extends — ADR-0040 scoped
  `trusty-mcp` to protocol primitives (JSON-RPC/MCP wire types) plus the
  gworkspace-hosting split. This ADR adds config ownership as a second,
  additive responsibility behind a default-off feature, not a reversal of the
  lean-rlib property ADR-0040 protects.
- **ADR-0042 (Static persistent MCP configuration), amended by ADR-0058 for
  the `trusty-code` boundary:** Amends — `trusty-mpm`'s user-scope Claude Code
  declarations stay static and persistent exactly as ADR-0042 describes. This
  ADR adds the shared-file case for `trusty-code`/`trusty-agents` and the
  per-assistant override tier, both of which ADR-0042 predates and neither of
  which existed when ADR-0058 amended it for `trusty-code` alone.
- **ADR-0058 (Trusty Code is an independent, product-owned harness), decision
  5:** Consistent — `trusty-code` resolves trusted executable MCP definitions
  under `~/.trusty-code/` privately and does not mutate `trusty-mpm`'s managed
  configuration. This ADR supplies the shared definition format ADR-0058
  assumed but did not itself define.
- No other prior decision addresses per-assistant MCP configuration.
