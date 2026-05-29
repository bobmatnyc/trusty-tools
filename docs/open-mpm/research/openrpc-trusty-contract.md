# OpenRPC over stdio contract

> **Status**: Specification — server side TBD in `trusty-common`. Client
> side is in place via `src/tools/registry/` (#453, commit `7710caf`).
>
> **Ticket**: open-mpm#455

## Background

`trusty-memory` and `trusty-search` historically shipped as stdio MCP
subprocesses spawned by `src/plugins/trusty_*` and managed by
`PluginManager`. The stdio path is preserved for backward compatibility,
but the preferred dispatch is now **OpenRPC** (https://spec.open-rpc.org/)
via the `direct` driver: an HTTP
JSON-RPC 2.0 server that advertises tools via `rpc.discover` and executes
them via `tools/call`.

This document is the **server-side API contract** the `trusty-common`
repo must implement so that `[[tool_registry.endpoints]]` entries in
`~/.open-mpm/config.toml` can talk to them.

## Transport

- **Protocol**: JSON-RPC 2.0 over HTTP POST
- **Path**: `/rpc` (single endpoint; routing by `method` field)
- **Content-Type**: `application/json`
- **Default ports**:
  - `trusty-memory`: **8732**
  - `trusty-search`: **8733**
- **Concurrency**: Server MUST support JSON-RPC 2.0 **Batch** requests
  (an array of envelopes in one POST) for `tools/call`.
- **CORS**: Localhost-only by default; no CORS required.

## Authentication

| Auth kind | Env var | Header |
|-----------|---------|--------|
| `bearer-env` | `TRUSTY_TOKEN` | `Authorization: Bearer <value>` |

The client (`DirectDriver`) reads the env var at startup and injects the
header on every request. The server SHOULD accept any non-empty token in
local-dev mode; production deployments MAY enforce a fixed token compared
constant-time.

When the env var is unset the client sends no Authorization header — the
server SHOULD allow this for local-dev convenience.

## Discovery: `rpc.discover`

The client calls `rpc.discover` once at startup (per endpoint) — the
result is cached for the endpoint's `discovery_ttl_secs` (open-mpm
defaults to `0` for these endpoints, meaning "cache for process
lifetime").

### Request

```json
{
  "jsonrpc": "2.0",
  "id": "<uuid>",
  "method": "rpc.discover",
  "params": {}
}
```

### Response

```json
{
  "jsonrpc": "2.0",
  "id": "<uuid>",
  "result": {
    "server": {
      "name": "trusty-memory",
      "version": "0.4.0"
    },
    "protocol_version": "openrpc/1",
    "capabilities": {
      "batch": true,
      "streaming": false
    },
    "tools": [
      {
        "name": "memory_remember",
        "description": "Persist a memory record with optional tags.",
        "scope": "memory.write",
        "input_schema": { "type": "object", "properties": { ... } },
        "output_schema": { "type": "object", "properties": { ... } },
        "idempotent": false,
        "side_effects": "write"
      },
      ...
    ]
  }
}
```

The `tools[].scope` string is critical: the open-mpm tool registry
filters tools whose scope does not match any of the operator-declared
`[[endpoints]].scopes` patterns. Scope patterns use `*` as a suffix
wildcard (`memory.*` matches `memory.read` and `memory.write`).

## Execution: `tools/call`

### Request

```json
{
  "jsonrpc": "2.0",
  "id": "<uuid>",
  "method": "tools/call",
  "params": {
    "name": "memory_recall",
    "arguments": {
      "query": "what does the user prefer for lunch",
      "limit": 5
    }
  }
}
```

### Response (success)

The result envelope mirrors the MCP `CallToolResult` shape so callers
can re-use the existing `extract_text_results` parsing code:

```json
{
  "jsonrpc": "2.0",
  "id": "<uuid>",
  "result": {
    "content": [
      {
        "type": "text",
        "text": "{\"id\":\"mem_42\",\"score\":0.91,\"text\":\"...\"}"
      },
      {
        "type": "text",
        "text": "{\"id\":\"mem_07\",\"score\":0.84,\"text\":\"...\"}"
      }
    ],
    "isError": false
  }
}
```

Servers SHOULD return one `content[]` entry per result row, with each
`text` field being a JSON-encoded structured payload. Non-JSON text is
allowed (the client falls back to `{"text": "..."}`).

### Response (tool-level error)

```json
{
  "jsonrpc": "2.0",
  "id": "<uuid>",
  "result": {
    "content": [
      { "type": "text", "text": "memory_recall failed: index not built" }
    ],
    "isError": true
  }
}
```

### Response (transport-level error)

Standard JSON-RPC 2.0 error envelope; surfaces as a driver error in the
client.

```json
{
  "jsonrpc": "2.0",
  "id": "<uuid>",
  "error": { "code": -32601, "message": "Method not found" }
}
```

## Required Tools

### `trusty-memory` (port 8732)

| Tool | Scope | Description | Side effects |
|------|-------|-------------|--------------|
| `memory_remember` | `memory.write` | Persist content with optional tags; return record id. | `write` |
| `memory_recall` | `memory.read` | Top-k semantic recall; return ranked records. | `read` |
| `memory_recall_deep` | `memory.read` | Multi-pass recall with re-ranking (slower, higher recall). | `read` |
| `memory_forget` | `memory.write` | Delete a record by id. | `write` |
| `memory_list` | `memory.read` | List recent records (optionally tag-filtered). | `read` |

### `trusty-search` (port 8733)

| Tool | Scope | Description | Side effects |
|------|-------|-------------|--------------|
| `search_code` | `search.read` | Hybrid semantic + literal search of the indexed corpus. | `read` |
| `search_similar` | `search.read` | Find chunks similar to a reference chunk id. | `read` |
| `index_status` | `search.read` | Report index size, freshness, file coverage. | `read` |

### `gworkspace` (stdio binary: `gworkspace-mcp`)

The Google Workspace endpoint is owned by the `trusty-gworkspace` crate in
`trusty-common` and ships as the `gworkspace-mcp` binary. It speaks the
same OpenRPC 1.3.2 envelope as the other endpoints, but each `methods[i]`
entry additionally carries an `x-google-scopes` extension array of Google
OAuth scope URLs (`https://www.googleapis.com/auth/...`) — used by the
host to prepare scope-aware OAuth flows. Open-mpm's scope-enforcement
layer uses dotted scope patterns instead (e.g. `google.gmail.*`); the
mapping between the two is internal to the gworkspace binary.

| Tool family | Open-mpm scope | Description |
|------|-------|-------------|
| `gmail_*` | `google.gmail.*` | Search/read/send/list Gmail messages, labels, settings. |
| `calendar_*` | `google.calendar.*` | List/create/update/delete events; list calendars. |
| `drive_*` | `google.drive.*` | Search/read/create/update Drive files. |
| `docs_*` | `google.docs.*` | Read/create/update Google Docs. |
| `sheets_*` | `google.sheets.*` | Read/write Google Sheets ranges and values. |
| `tasks_*` | `google.tasks.*` | List/create/complete Google Tasks. |

Operators authenticate once via `gworkspace-mcp auth login` (which writes
OAuth tokens to the user's keychain); subsequent stdio sessions reuse the
stored tokens. Enable in `~/.open-mpm/config.toml` with:

```toml
[[tool_registry.endpoints]]
name = "gworkspace"
driver = "direct"
command = "gworkspace-mcp"
enabled = true
scopes = ["google.gmail.*", "google.calendar.*", "google.drive.*",
          "google.docs.*", "google.sheets.*", "google.tasks.*"]
```

Servers MAY expose additional tools beyond this list; tools whose scope
falls outside the operator-declared `scopes` patterns in
`~/.open-mpm/config.toml` will be filtered out at discovery time.

## Operator Configuration

Once the server side is running, operators flip `enabled = true` in the
auto-generated `~/.open-mpm/config.toml`:

```toml
[[tool_registry.endpoints]]
name = "trusty-memory"
driver = "direct"
url = "http://127.0.0.1:8732/rpc"
enabled = true
scopes = ["memory.read", "memory.write"]

[tool_registry.endpoints.transport]
timeout_ms = 5000

[tool_registry.endpoints.auth]
kind = "bearer-env"
env = "TRUSTY_TOKEN"
```

The harness re-loads this file on every prompt build, so flipping
`enabled` does not require a restart.

## Stdio Fallback

When the direct endpoint is disabled (the default), the existing
`src/plugins/trusty_memory.rs` and `src/plugins/trusty_search.rs`
wrappers continue to work as before. Both paths can coexist; the
`ToolRegistry::register` call panics on duplicate tool names in debug
builds, so operators must not enable both transports for the same tool
surface simultaneously.

## Migration Plan (server side, owned by `trusty-common`)

1. Add a `jsonrpsee`-based HTTP server (or hand-rolled with `axum`) to
   each binary, gated behind a `serve-rpc` subcommand and a `--port`
   flag.
2. Implement `rpc.discover` returning the tool manifest above.
3. Re-implement each tool's `tools/call` handler by sharing the existing
   business-logic crate between the stdio and HTTP entry points.
4. Add an integration test that POSTs to `/rpc` and verifies the
   manifest + a round-trip `memory_remember` / `memory_recall`.
5. Cut a release; operators install the new binary and flip
   `enabled = true` in `~/.open-mpm/config.toml`.

## See Also

- `src/tools/registry/` — open-mpm client implementation
- `src/tools/registry/direct.rs` — HTTP JSON-RPC 2.0 driver
- `src/tools/registry/discovery.rs` — manifest types & TTL cache
- `src/tools/registry/scope.rs` — scope pattern matching
- `src/plugins/trusty_memory.rs` / `src/plugins/trusty_search.rs` —
  legacy stdio MCP wrappers (kept for backward compatibility)
