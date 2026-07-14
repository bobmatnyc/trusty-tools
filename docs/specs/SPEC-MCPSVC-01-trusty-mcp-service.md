# DOC-27 — `trusty-mcp-service`: Unified Native Sidecar Services via MCP-A

> **⚠️ SUPERSEDED BY ADR-0014 (native Rust MCP, PR #2624).** The unified
> `trusty-mcp-service` / MCP-A gateway approach described below was **not**
> adopted. Its intent — retiring the out-of-band Python sidecars (`gworkspace-mcp`,
> `slack-mpm`) in favor of native Rust MCP servers — is instead delivered directly:
> each service ships as its own native stdio MCP binary in-workspace (e.g. the
> `gworkspace-mcp` binary in crate `trusty-gworkspace`), wired into
> `trusty-agents`' default MCP config rather than through a single MCP-A gateway.
> This document is retained for historical/design context only; do not implement
> against it. See the closed tracking work under #1606.

**Status:** Superseded (was: Draft)
**Subsystem:** trusty-mcp-service (new) — unified native sidecar services / MCP-A gateway
**Owner:** Engineering (Bob Matsuoka)
**Last-updated:** 2026-06-23
**Spec ID:** `SPEC-MCPSVC-01~draft` … `-07~draft` (DOC-27)
**Builds on:** `trusty_common::mcp` (JSON-RPC framing, `run_stdio_loop`, `ServiceDescriptor`,
`DaemonBridgeConfig`, `ensure_daemon_up`, `OpenRpcBuilder`, `shutdown_signal`,
`server::with_standard_middleware`); DOC-19 — TELUI / Telegram (the `trusty-mpm-telegram`
binary and `telegram` feature that SPEC-MCPSVC-05 moves into this crate);
DOC-24 — Standalone Managed `trusty-mpm` Driver (`docs/specs/standalone-managed-trusty-mpm.md`,
the MCP wiring model that SPEC-MCPSVC-06 extends).
**Cross-ref:** `crates/trusty-common/src/mcp/` (framing primitives);
`crates/trusty-analyze/src/mcp/stdio.rs` (`guard_response_size` pattern to adopt);
`crates/trusty-mpm/src/core/session_launch/settings.rs` (consumer injection point — SPEC-MCPSVC-06);
`crates/trusty-agents/src/plugins/` (`StdioMcpClient`, `PluginManager` — SPEC-MCPSVC-06);
`crates/trusty-code/src/tools/` (tool provider injection — SPEC-MCPSVC-06);
MCP-A protocol spec and JSON Schemas: `~/Projects/mcp-a-protocol/` (v1.0.1-beta);
prior-art Python projects: `~/Projects/gworkspace-mcp/` (Google Workspace),
`~/Projects/slack-mpm/` (Slack).

> **Scope note.** This is a **behavior contract** for a new shared Rust crate `trusty-mcp-service`
> that houses **all local native sidecar services** for the trusty agent ecosystem — specifically
> `trusty-gworkspace` (Google Workspace), `trusty-slack` (Slack), and `trusty-telegram`
> (moved from `trusty-mpm`). Each service domain is surfaced to MCP via the **MCP-A protocol**:
> seven uniform primitives instead of a per-operation tool explosion. This spec is
> **design-stage only** — the PR that carries it opens **no Rust code, no Cargo.toml changes**.
> Implementation is tracked under the phased work items in §12.

---

## Purpose & Scope

Today the trusty agent ecosystem relies on Python-process sidecars (`gworkspace-mcp`,
`slack-mpm`) launched out-of-band and wired manually into each consumer's `.mcp.json`.
These sidecars share no Rust type system, provide no boundary validation, expose a flat
explosion of per-operation MCP tools, and require N separate processes for 3 consumers
(trusty-mpm, trusty-code, trusty-agents). The `trusty-mpm-telegram` alert path is also
stranded inside `trusty-mpm` behind a feature gate, making it unavailable to other consumers.

`trusty-mcp-service` consolidates all three native sidecar services into a single Rust crate
inside the trusty-tools workspace. The MCP surface is structured under the **MCP-A protocol**
(v1.0.1-beta): 7 stable primitives that every consumer calls the same way regardless of which
backend domains are enabled. Adding a new backend is adding a new domain — zero client change.

**In scope:** crate layout and module decomposition; the MCP-A primitive surface and domain
registry; domain specs for `trusty-gworkspace`, `trusty-slack`, `trusty-telegram`; core Rust
trait interfaces (design sketches); transport and deployment model; consumer integration points;
key decisions and phased work items.

**Out of scope:** implementation (no code); the MCP-A protocol itself (consumed as-is);
changes to `trusty-common`, `trusty-mpm`, `trusty-agents`, or `trusty-code` beyond the
injection-point wiring described in §9; ONNX embeddings, search indexing, or memory palace
storage.

---

## Table of Contents

| ID | Section | Implementing module(s) |
|----|---------|------------------------|
| SPEC-MCPSVC-01~draft | [Framework & MCP-A Primitive Surface](#1-framework--mcp-a-primitive-surface-spec-mcpsvc-01draft) | `trusty_mcp_service::mcp_a`, `::registry`, `::dispatch` |
| SPEC-MCPSVC-02~draft | [Auth & HTTP Foundation](#2-auth--http-foundation-spec-mcpsvc-02draft) | `trusty_mcp_service::auth`, `::http`, `::credential` |
| SPEC-MCPSVC-03~draft | [`trusty-gworkspace` Domain](#3-trusty-gworkspace-domain-spec-mcpsvc-03draft) | `trusty_mcp_service::domains::gworkspace` |
| SPEC-MCPSVC-04~draft | [`trusty-slack` Domain](#4-trusty-slack-domain-spec-mcpsvc-04draft) | `trusty_mcp_service::domains::slack` |
| SPEC-MCPSVC-05~draft | [`trusty-telegram` Domain (move from trusty-mpm)](#5-trusty-telegram-domain-spec-mcpsvc-05draft) | `trusty_mcp_service::domains::telegram` |
| SPEC-MCPSVC-06~draft | [Consumer Integration](#6-consumer-integration-spec-mcpsvc-06draft) | `trusty-mpm` settings, `trusty-agents` plugins, `trusty-code` tools |
| SPEC-MCPSVC-07~draft | [Deployment & Transport](#7-deployment--transport-spec-mcpsvc-07draft) | `trusty_mcp_service::bin`, daemon + stdio proxy |

---

## 1. Framework & MCP-A Primitive Surface {#SPEC-MCPSVC-01~draft}

### 1.1 Problem and Goals

The prior Python sidecars expose one MCP tool per operation: `gmail_send`, `gmail_list`,
`calendar_create`, `drive_upload`, etc. — easily 40–80 tools across three services. LLM
tool routing degrades with large tool counts: the model must choose among 80 near-identical
names. MCP-A collapses the surface to **exactly 7 primitives** that every domain maps into.
Consumers branch on domain names and error code strings, not tool names. Domains added later
are invisible to the client routing layer.

A secondary goal is structural typing: prior projects had no boundary validation (hand-written
`dict` schemas, no runtime checks). Rust `#[derive(serde::Deserialize, schemars::JsonSchema)]`
on every input struct gives both free `inputSchema` generation for MCP tool registration and
compile-time assurance of exhaustive field handling.

### 1.2 The 7 MCP-A Primitives

Conformance target: **Full** (all 7 + structured-response + hierarchical schema + deterministic
aggregations). Rationale: the three domains all have rich entity models and state-changing
actions; Core-only (query + context) would omit schema introspection needed by tool-calling
agents and the action pipeline needed for side-effects like sending messages or creating events.

| Primitive | Purpose | Key fields |
|-----------|---------|------------|
| `discover` | RBAC-filtered catalog of enabled domains + server capability block | `domains[]` with `name`, `version`, `capabilities`; optional `filter` for RBAC scope |
| `schema` | Domain ontology: entities, fields, types, units, allowed_aggregations, relationships | `domain`, optional `entity` for hierarchical drill |
| `query` | NL question → classify → fan-out → consolidated answer; optional `response_schema` for typed output | `question`, `domains[]`, `response_schema?`; returns `answer_id` |
| `follow_up` | Drill / refine / poll against an existing `answer_id` without re-classifying | `answer_id`, `question` |
| `context` | Read/write user identity, preferences, scope; discriminated by `action` field | `action` (`read`/`write`/`clear`), `scope?`, `value?` |
| `explain` | Routing decision, sources, confidence, latency for a given `answer_id` or `action_id` | `id` (answer or action) |
| `action` | State-changing NL request; clarification rounds; stable `action_id`; RBAC re-checked pre-apply | `intent`, `domain`, `action_id?` (resume); returns clarification or result |

**Conformance gate (Aggregation Correctness):** `allowed_aggregations` returned by `schema` must
be computed deterministically server-side, never LLM-estimated. This is a hard MCP-A Full
conformance requirement.

### 1.3 Error Model

MCP-A defines 11 abstract error codes. All operational failures are returned **in-band** as
`content` with an `"operational": true` flag so the consuming agent can reason about them;
only protocol/transport errors propagate to the JSON-RPC error channel.

| MCP-A code | When raised |
|------------|------------|
| `UNAUTHENTICATED` | No valid credentials for the domain |
| `FORBIDDEN` | Authenticated but RBAC denies the operation |
| `INVALID_REQUEST` | Malformed input, schema violation |
| `DOMAIN_NOT_FOUND` | Requested domain not registered or not enabled |
| `ANSWER_NOT_FOUND` | `answer_id` / `action_id` expired or unknown |
| `SCHEMA_NONCONFORMANT` | `response_schema` violates domain schema constraints |
| `AGGREGATION_NOT_ALLOWED` | Requested aggregation not in `allowed_aggregations` |
| `TIMEOUT` | Backend call exceeded deadline |
| `SOURCE_UNAVAILABLE` | Domain backend unreachable (API down, network error) |
| `ACTION_NOT_FOUND` | `action_id` not found for `follow_up` or `explain` |
| `ACTION_FAILED` | Action reached the apply phase but the backend rejected it |

### 1.4 Answer/Action Store

Both `query` and `action` return durable IDs consumed by `follow_up` and `explain`.
The store is an in-process `HashMap<Uuid, RoutingDecision>` protected by `Arc<RwLock<_>>`,
with a configurable TTL (default 15 minutes) and bounded capacity (default 1 000 entries,
LRU eviction). The store is not persisted across daemon restarts — `ANSWER_NOT_FOUND` is
the expected response after restart.

---

## 2. Auth & HTTP Foundation {#SPEC-MCPSVC-02~draft}

### 2.1 `AuthProvider` Trait

Auth is injected at domain-construction time via `Arc<dyn AuthProvider + Send + Sync>`. This
keeps domain logic free of credential I/O and makes unit tests injectable without network calls.

```rust
// ILLUSTRATIVE — not final. Intended to convey the contract, not the implementation.

/// Why: decouples credential I/O from domain logic; makes testing injectable.
/// What: async trait that yields a bearer token string on demand; implementations
///   may refresh expired tokens transparently.
/// Test: mock implementations return fixed tokens; integration tests hit live OAuth.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    async fn bearer_token(&self) -> Result<String, AuthError>;
    async fn invalidate(&self);  // called on 401 to force refresh
}
```

### 2.2 Two-Tier Credential Storage

Credentials are resolved in priority order:

1. **Project-local** — `<cwd>/.trusty-mcp-service/<domain>/credentials.json` (0o600).
   Walk CWD → ancestors → project root (detected by `.git`) → home env.
2. **User-home** — `~/.trusty-mcp-service/<domain>/credentials.json` (0o600).

The `<domain>/` directory itself is 0o700. This mirrors the `trusty-mpm` credential model
and handles GUI MCP hosts that launch with `CWD=/`.

### 2.3 OAuth2 PKCE Flow (gworkspace)

`find_free_port()` probes the OS for an available TCP port for the OAuth2 callback listener.
The PKCE verifier/challenge pair is generated in-process. After callback, tokens are written
to the two-tier path at tier 1 by default.

### 2.4 HTTP Retry Layer

- **401** → call `AuthProvider::invalidate()`, refresh token, retry once.
- **429** → read `Retry-After` header (seconds), sleep, retry up to 3 times.
- All other 4xx → propagate as `SOURCE_UNAVAILABLE` or `ACTION_FAILED` (domain-specific).

### 2.5 Cursor Pagination

A generic `Stream`-based paginator drives all list operations: given a `next_page_token`
field name and a typed page struct, it yields items lazily and stops when the token is absent.
Domains register their paginator type; callers collect or stream as appropriate.

### 2.6 Token Masking

`mask(token: &str) -> String` returns the first 4 and last 4 characters with `****` in between
for any token longer than 12 characters; shorter tokens become `****`. Used in all log and
display paths involving credentials.

---

## 3. `trusty-gworkspace` Domain {#SPEC-MCPSVC-03~draft}

### 3.1 Origin

Ported from `~/Projects/gworkspace-mcp/` (Python/FastMCP). The Python project exposes one
tool per operation with hand-written JSON schemas and no runtime validation. The Rust port
gains typed structs, boundary validation, and MCP-A routing.

### 3.2 Capabilities

| Sub-domain | Key entities | Supported via MCP-A |
|------------|-------------|---------------------|
| Gmail | Message, Thread, Label, Attachment, Draft | `query` (search/read), `action` (send/reply/label/delete/draft) |
| Calendar | Event, Calendar, Attendee, FreeBusy | `query` (search/list/freebusy), `action` (create/update/delete/accept) |
| Drive | File, Folder, Permission, Comment | `query` (search/list/get), `action` (create/upload/move/delete/share) |
| Docs | Document, Revision | `query` (read/list), `action` (create/update/append) |
| Sheets | Spreadsheet, Sheet, Range | `query` (read range/metadata), `action` (write range/create sheet) |
| Slides | Presentation, Slide | `query` (read), `action` (create/update) |
| Tasks | Task, TaskList | `query` (list), `action` (create/complete/delete) |

### 3.3 Multi-Account Support

Multiple Google accounts are distinguished by an `account` field in the `context` primitive
(`action: "write"`, `scope: "account"`, `value: "<email>"`). The domain maintains a per-account
`AuthProvider` map keyed by email. The `discover` primitive lists available accounts in the
capability block.

### 3.4 Auth Model

OAuth2 PKCE (§2.3). Scopes: Gmail read+send, Calendar read+write, Drive read+write,
Docs/Sheets/Slides/Tasks read+write. Consent screen configuration, client ID, and client
secret are read from environment variables (`GWORKSPACE_CLIENT_ID`, `GWORKSPACE_CLIENT_SECRET`)
or from the two-tier credential store (`credentials.json` in the domain directory).

### 3.5 Schema Declaration

The `schema` primitive for this domain returns:

- **Entities**: `Message`, `Event`, `File`, `Document`, `Spreadsheet`, `Presentation`, `Task`,
  `Thread`, `Label`, `Calendar`, `Folder`, `TaskList`
- **Relationships**: `Thread contains Message`, `Calendar contains Event`, `Folder contains File`
- **Allowed aggregations** (deterministic): `count`, `date_histogram` (by day/week/month),
  `label_distribution` (Gmail only)

---

## 4. `trusty-slack` Domain {#SPEC-MCPSVC-04~draft}

### 4.1 Origin

Ported from `~/Projects/slack-mpm/` (Python). Same pain points as gworkspace: flat tool list,
no validation, Python process.

### 4.2 Capabilities

| Sub-domain | Key entities | Supported via MCP-A |
|------------|-------------|---------------------|
| Channels | Channel, ChannelMember | `query` (list/info/history), `action` (create/invite/archive/join) |
| Messages | Message, Reaction, Thread | `query` (search/list/thread), `action` (post/reply/update/delete/react) |
| Users | User, UserGroup, Presence | `query` (list/info/presence) |
| Files | File | `query` (list/info), `action` (upload/delete/share) |
| Workspace | WorkspaceInfo, Emoji, Team | `query` (info/emoji list) |
| Reminders | Reminder | `query` (list), `action` (add/delete/complete) |
| Bookmarks | Bookmark | `query` (list), `action` (add/edit/remove) |
| Scheduled Messages | ScheduledMessage | `query` (list), `action` (create/delete) |

### 4.3 Auth Model

Static **bot token** (`SLACK_BOT_TOKEN`) and **user token** (`SLACK_USER_TOKEN`) read from
environment variables or the two-tier credential store. No OAuth2 flow required at runtime
(tokens are provisioned by the workspace admin). Bot token is used for most operations; user
token is used for user-context operations (e.g. setting user presence, posting as user).

### 4.4 Schema Declaration

The `schema` primitive returns:

- **Entities**: `Message`, `Channel`, `User`, `File`, `Reaction`, `Bookmark`, `Reminder`,
  `ScheduledMessage`, `Thread`, `UserGroup`
- **Relationships**: `Channel contains Message`, `Message has Thread`, `User belongs to UserGroup`
- **Allowed aggregations**: `count`, `date_histogram` (by day), `channel_distribution`,
  `user_distribution`, `reaction_distribution`

---

## 5. `trusty-telegram` Domain (move from trusty-mpm) {#SPEC-MCPSVC-05~draft}

### 5.1 What Moves

The current `trusty-mpm` `telegram` feature gates (`teloxide`, `tokio-util`) and the
`crates/trusty-mpm/src/telegram/` module tree (alerts, commands, formatter, supervisor,
tests) are **extracted** into `trusty-mcp-service` as the `trusty-telegram` domain. The
`trusty-mpm` `telegram` and `cli` feature gates are updated to depend on the
`trusty-mcp-service` re-export rather than the in-tree module.

The **24/7 alert path** (`alerts.rs`, `run_alert_loop`) is preserved exactly. trusty-mpm
continues to start the alert loop on daemon boot; the difference is that the loop code
lives in `trusty-mcp-service`.

### 5.2 Why Move

- The Telegram control surface (and the alert loop) is useful to `trusty-agents` and
  `trusty-code` independently of `trusty-mpm`, but today neither can use it without
  pulling in the entire `trusty-mpm` dep tree.
- Collocating with gworkspace and slack makes the single-daemon deployment model (§7)
  viable: one process handles all three domain types.
- TELUI (DOC-19) specifies the Telegram UX contract; the alert loop is an implementation
  detail of trusty-mpm's session observability, not of the Telegram domain itself.

### 5.3 Capabilities

| Sub-domain | Key entities | Supported via MCP-A |
|------------|-------------|---------------------|
| Messages | Message, Chat | `query` (poll inbox), `action` (send/edit/delete message) |
| Bot | BotInfo, Command | `query` (info), `action` (set commands) |
| Alerts | Alert | `action` (dispatch alert to authorized chat) |

### 5.4 Auth Model

Static bot token (`TELEGRAM_BOT_TOKEN`). Single-operator (`BotOptions::allowed_user_id`).
The move preserves the existing `resolve_token` / `read_dotenv_key` resolution: `.env.local`
in the project root, then `TELEGRAM_BOT_TOKEN` env var.

### 5.5 Alert Path Preservation Contract

- `run_alert_loop` remains callable from `trusty-mpm` with the same signature it has today.
- The loop is started by the daemon on boot exactly as it is now.
- No behavioral change to alert delivery, polling cadence, or authorization gate.

---

## 6. Consumer Integration {#SPEC-MCPSVC-06~draft}

### 6.1 trusty-mpm

Inject a `trusty-mcp-service` MCP-A server block in session-launch settings alongside the
existing `inject_trusty_memory_mcp` / `inject_trusty_search_mcp` calls.

**File:** `crates/trusty-mpm/src/core/session_launch/settings.rs`
**New function:** `inject_trusty_mcp_service_mcp(config: &mut ClaudeConfig, …)`

The injected block follows the same `"command": "trusty-mcp-serviced", "args": ["serve", "--stdio"]`
pattern as trusty-memory. The daemon is started via `ensure_daemon_up` (the `DaemonBridgeConfig`
pattern from `trusty-common`).

### 6.2 trusty-agents

Add a new `Plugin` in `crates/trusty-agents/src/plugins/` that spawns the MCP-A stdio proxy
via `StdioMcpClient::spawn()` and registers it through `PluginManager::init()`. The plugin
degrades gracefully: if the `trusty-mcp-serviced` binary is not on PATH, the plugin logs
`warn!` and reports `PluginStatus::Unavailable` — sessions continue without the domain tools.

**File:** `crates/trusty-agents/src/plugins/trusty_mcp_service.rs` (new)

### 6.3 trusty-code

Add a new tool provider in `crates/trusty-code/src/tools/` that wraps the MCP-A stdio proxy.
The provider is optional: enabled when `trusty-mcp-serviced` is detected on PATH; absent
otherwise. It exposes the 7 MCP-A primitives as `trusty-code` tool entries.

**File:** `crates/trusty-code/src/tools/mcp_service.rs` (new)

---

## 7. Deployment & Transport {#SPEC-MCPSVC-07~draft}

### 7.1 Recommended: Single Daemon + stdio Proxy

**Model:** one `trusty-mcp-serviced` daemon binary serves all enabled domains over axum HTTP
(localhost only, configurable port). A thin `trusty-mcp-serviced serve --stdio` MCP proxy
mode forwards JSON-RPC 2.0 requests from the Claude Code process to the daemon and
auto-reconnects with exponential backoff if the daemon restarts.

This mirrors the trusty-memory model exactly:
- `trusty-memory` → `trusty-mcp-serviced`
- `trusty-memory serve --stdio` → `trusty-mcp-serviced serve --stdio`
- `ensure_daemon_up` / `DaemonBridgeConfig` from `trusty-common` handles daemon lifecycle.

**Rationale for the daemon model over N separate sidecar processes:**
- 3 consumers × 3 domains = up to 9 separate processes without the daemon model; the daemon
  collapses that to 1 daemon + 1 stdio proxy per consumer session.
- The daemon owns the answer/action store (§1.4) and auth token cache — shared correctly
  across concurrent consumer sessions.
- HTTP health probes, graceful SIGTERM drain, and `launchd`/systemd supervision are all
  standard patterns already in `trusty-common`.
- The stdio proxy is ~50 lines of proxy code (the `trusty-memory` proxy is the template).

**Alternative: in-process dispatch (trusty-search / trusty-analyze model)**

Each consumer links `trusty-mcp-service` as a library and dispatches domain calls in-process,
with no daemon. Trade-offs:

| Factor | Daemon model | In-process model |
|--------|-------------|-----------------|
| Answer/action store | Shared across sessions | Per-process, not shared |
| Auth token cache | Shared (one OAuth flow) | Per-process (N OAuth flows) |
| Operational complexity | 1 daemon to supervise | No extra process |
| Memory footprint | 1 daemon + lightweight proxy | Domain code in every consumer process |
| Startup latency | Daemon must be up first | Immediate |

**Recommendation:** daemon model for v1. The shared auth token cache is the deciding factor:
OAuth2 PKCE interactive flows must happen only once per account per daemon lifetime, not once
per consumer session.

### 7.2 Binaries

| Binary | Purpose |
|--------|---------|
| `trusty-mcp-serviced` | Long-running daemon (axum HTTP, all enabled domains) |
| `trusty-mcp-serviced serve --stdio` | MCP stdio proxy (newline-delimited JSON-RPC 2.0 → HTTP) |

Both binaries live in `crates/trusty-mcp-service/src/bin/`.

### 7.3 Transport Contract

- **Framing:** newline-delimited JSON-RPC 2.0 on stdio (the trusty standard).
- **Response size guard:** adopt `guard_response_size()` from `trusty-analyze`
  (`crates/trusty-analyze/src/mcp/stdio.rs`) — 2 MB ceiling, `TRUSTY_MCP_MAX_RESPONSE_BYTES`
  override. Crate-local copy or promote to `trusty-common::mcp` (open question, §11).
- **Graceful shutdown:** `trusty_common::shutdown_signal()` (SIGTERM drain) in the daemon binary.
- **Standard middleware:** `trusty_common::server::with_standard_middleware` on all axum routes.
- **Logs to stderr only** — stdout is reserved for JSON-RPC framing (trusty convention).

---

## 8. Crate Layout {#crate-layout}

```
crates/trusty-mcp-service/
├── Cargo.toml                    # edition = "2021", thiserror (lib) + anyhow (bins)
├── src/
│   ├── lib.rs                    # pub re-exports; feature gates for optional domains
│   ├── mcp_a/                    # MCP-A primitive layer
│   │   ├── mod.rs                # the 7 tool handlers (discover/schema/query/…)
│   │   ├── primitives.rs         # input/output types, #[derive(Deserialize, JsonSchema)]
│   │   ├── error.rs              # McpError (thiserror), MCP-A 11-code enum
│   │   └── store.rs              # answer/action id store (Arc<RwLock<HashMap>>)
│   ├── registry/
│   │   ├── mod.rs                # DomainRegistry, register/lookup/list
│   │   └── descriptor.rs         # DomainDescriptor (name, version, capabilities)
│   ├── dispatch/
│   │   └── mod.rs                # Arc<HashMap<String, BoxedHandler>> dispatch table
│   ├── auth/
│   │   ├── mod.rs                # AuthProvider trait, AuthError (thiserror)
│   │   ├── credential.rs         # two-tier credential storage, mask()
│   │   ├── oauth_pkce.rs         # PKCE flow, find_free_port, token storage
│   │   └── token_cache.rs        # per-account token cache (Arc<RwLock<_>>)
│   ├── http/
│   │   ├── mod.rs                # shared HTTP client (reqwest), retry layer
│   │   └── paginator.rs          # generic cursor Stream paginator
│   ├── context/
│   │   └── mod.rs                # RequestContext, tokio::task_local! scope injection
│   ├── domains/
│   │   ├── mod.rs                # Domain trait; register_all() convenience fn
│   │   ├── gworkspace/           # trusty-gworkspace (feature = "gworkspace")
│   │   │   ├── mod.rs
│   │   │   ├── gmail.rs
│   │   │   ├── calendar.rs
│   │   │   ├── drive.rs
│   │   │   ├── docs.rs
│   │   │   ├── sheets.rs
│   │   │   ├── slides.rs
│   │   │   └── tasks.rs
│   │   ├── slack/                # trusty-slack (feature = "slack")
│   │   │   ├── mod.rs
│   │   │   ├── channels.rs
│   │   │   ├── messages.rs
│   │   │   ├── users.rs
│   │   │   ├── files.rs
│   │   │   ├── workspace.rs
│   │   │   ├── reminders.rs
│   │   │   ├── bookmarks.rs
│   │   │   └── scheduled.rs
│   │   └── telegram/             # trusty-telegram (feature = "telegram")
│   │       ├── mod.rs            # moved from crates/trusty-mpm/src/telegram/
│   │       ├── alerts.rs
│   │       ├── commands.rs
│   │       ├── formatter/
│   │       └── supervisor.rs
│   └── server/
│       └── mod.rs                # axum router wiring, health endpoint (feature = "axum-server")
├── src/bin/
│   ├── serviced.rs               # trusty-mcp-serviced: daemon binary (anyhow)
│   └── stdio_proxy.rs            # serve --stdio proxy mode (anyhow)
└── tests/
    ├── mcp_a_conformance.rs      # fixture-driven conformance tests vs ~/Projects/mcp-a-protocol/examples/
    ├── gworkspace_integration.rs # #[ignore] live API tests
    ├── slack_integration.rs      # #[ignore] live API tests
    └── telegram_integration.rs   # #[ignore] live API tests
```

**File size discipline:** every `src/` `.rs` file is designed to stay under the 500 SLOC
production cap from day one. Sub-domain files (`gmail.rs`, `channels.rs`, etc.) are split by
operation group, not crammed into a single file. The `tests/` directory follows the 1500 SLOC
cap. Checked by `scripts/check_line_cap.sh` in CI.

---

## 9. Core Rust Interface Sketches

> **These are illustrative design sketches, not final API.** They are intended to communicate
> intent and constraints to the implementing engineer, not to prescribe implementation details.

### 9.1 `Domain` Trait

```rust
// ILLUSTRATIVE — not final.

/// Why: defines the unit of capability in the MCP-A domain registry; each backend
///   implements this trait to plug into discover/schema/query/action routing.
/// What: supplies metadata (name, version, capabilities) and async handlers for
///   schema queries and NL classification; handlers are registered separately via
///   `handlers()`.
/// Test: mock domains in unit tests; conformance fixtures cover the wire contract.
#[async_trait]
pub trait Domain: Send + Sync {
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn capabilities(&self) -> DomainCapabilities;

    /// Returns the domain ontology for the `schema` primitive.
    async fn schema(&self, entity: Option<&str>) -> Result<DomainSchema, McpError>;

    /// Classifies a NL question into typed operations for fan-out.
    async fn classify_query(&self, question: &str, ctx: &RequestContext)
        -> Result<Vec<ClassifiedOp>, McpError>;

    /// Returns the handler dispatch table for this domain's operations.
    fn handlers(&self) -> Arc<HashMap<String, BoxedHandler>>;
}
```

### 9.2 `McpError` Enum

```rust
// ILLUSTRATIVE — not final.

/// Why: separates operational errors (agent-reasoned, returned in-band) from
///   internal/programmer errors (logged, surfaced as protocol errors).
/// What: two variants; Operational carries the MCP-A 11-code enum for in-band
///   signalling; Internal wraps anyhow::Error for unexpected failures.
/// Test: unit tests assert Operational variants map to the correct MCP-A code names.
#[derive(thiserror::Error, Debug)]
pub enum McpError {
    #[error("operational error ({code}): {message}")]
    Operational { code: McpaErrorCode, message: String },

    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

/// MCP-A abstract error codes (§1.3). Branch on the name string, not ordinal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum McpaErrorCode {
    Unauthenticated,
    Forbidden,
    InvalidRequest,
    DomainNotFound,
    AnswerNotFound,
    SchemaNonconformant,
    AggregationNotAllowed,
    Timeout,
    SourceUnavailable,
    ActionNotFound,
    ActionFailed,
}
```

### 9.3 `ToolResult` Enum

```rust
// ILLUSTRATIVE — not final.

/// Why: uniform return type across all domain operation handlers; Content variant
///   carries the JSON value that becomes MCP content; the Operational error path
///   is represented as a Text content item with operational=true metadata.
/// What: three variants matching MCP content kinds; From<serde_json::Value> for
///   the common case of returning a typed serialized struct.
/// Test: unit tests assert each variant serializes to the correct MCP content shape.
pub enum ToolResult {
    Text(String),
    Json(serde_json::Value),
    Multi(Vec<ToolResult>),
}

impl From<serde_json::Value> for ToolResult {
    fn from(v: serde_json::Value) -> Self { ToolResult::Json(v) }
}
```

### 9.4 `RequestContext`

```rust
// ILLUSTRATIVE — not final.

/// Why: carries per-request identity and scope without threading them through every
///   function signature; tokio::task_local! makes it available in async contexts.
/// What: holds resolved user identity, RBAC scope, account selector, and trace id.
/// Test: task_local injection verified in domain handler unit tests.
pub struct RequestContext {
    pub user_id: Option<String>,
    pub account: Option<String>,      // for multi-account gworkspace
    pub scope: Vec<String>,           // RBAC scope tags
    pub trace_id: uuid::Uuid,
}

tokio::task_local! {
    pub static REQUEST_CONTEXT: RequestContext;
}
```

---

## 10. Key Decisions

### D-1: Single Crate Owns Binaries (override of issue-#1318 convention)

**Convention (issue #1318):** library crates are lib-only; binaries live in their own
crates with a single owner.

**This crate overrides that convention** for `trusty-mcp-service`. The binaries
(`trusty-mcp-serviced`, `serve --stdio`) are tightly coupled to the domain registry and
MCP-A dispatch that live in the library: they are thin wrappers over `lib.rs` entry points,
not independent programs. Splitting them into separate crates would create a circular dep
or require publishing the library first on every change — defeating the purpose of the
monorepo. `trusty-mcp-service` is the **one shared home for native sidecar services**;
this crate pattern (lib + bins collocated) is the norm for daemon crates in this workspace
(see `trusty-search`, `trusty-memory`, `trusty-analyze`). The issue-#1318 convention
targets application-level binaries, not daemon infrastructure crates.

### D-2: Edition `"2021"` for the Library

The library does not need let-chains (a 2024-only feature). `edition = "2021"` keeps it
consistent with `trusty-common`, `trusty-search`, `trusty-memory`, and `trusty-analyze`.
Binary entrypoints (`src/bin/`) follow the crate edition. If let-chains become desirable
later, the edition bump is a one-line change.

### D-3: Deployment Model — Daemon + stdio Proxy

Chosen over in-process dispatch (§7.1). The deciding factor is the shared auth token cache:
OAuth2 PKCE interactive flows must happen only once per account, not per consumer session.
In-process dispatch would require each consumer session to independently manage OAuth flows.
The daemon model is also consistent with trusty-memory and trusty-search — operators and
`launchd` plist recipes already exist for this pattern.

### D-4: MCP-A Full Conformance Tier

Chosen over Core-only. All three domains have rich entity models (`schema` is needed for
agent introspection), state-changing operations (`action` is needed for Gmail send, Slack
post, Telegram alert), and drilling workflows (`follow_up` is needed for multi-step
Calendar free-busy checks). Implementing only Core would require per-operation tools anyway,
negating the MCP-A surface reduction.

### D-5: Schema Codegen via `schemars` (not external codegen)

MCP-A JSON Schemas (in `~/Projects/mcp-a-protocol/schemas/*.json`) are used as **conformance
fixtures** in integration tests, not as a codegen source. Input/output types are written as
Rust structs with `#[derive(serde::Deserialize, schemars::JsonSchema)]`; `schemars` generates
the `inputSchema` block for MCP tool registration. This avoids a codegen build step, keeps
types in Rust, and gives compile-time exhaustiveness. Conformance tests (`tests/mcp_a_conformance.rs`)
validate the generated schemas against the MCP-A JSON Schema fixtures using `jsonschema` crate.

### D-6: `trusty-telegram` Move Strategy

The move is a **copy-then-delete** within the same PR: copy `crates/trusty-mpm/src/telegram/`
into `trusty_mcp_service::domains::telegram`, update `trusty-mpm/src/lib.rs` to re-export
from `trusty-mcp-service`, and remove the now-redundant in-tree copy. The `trusty-mpm` feature
gate `telegram` is updated to depend on `trusty-mcp-service/telegram` feature rather than the
in-tree module. The alert-path preservation contract (§5.5) is verified by running
`cargo test -p trusty-mpm -- telegram` after the move.

---

## 11. Open Questions

| # | Question | Owner | Notes |
|---|----------|-------|-------|
| OQ-1 | Should `guard_response_size` be promoted from `trusty-analyze` to `trusty-common::mcp` or copied locally? | Engineering | Promote is cleaner; copy avoids `trusty-common` churn. Decide before WI-1. |
| OQ-2 | Does `trusty-mpm`'s `cli` feature still bundle `telegram` directly, or does it become a dep on `trusty-mcp-service`? | trusty-mpm owner | Affects WI-5 scope. Moving to dep is cleaner long-term. |
| OQ-3 | Port of `teloxide` dep: should `trusty-mpm` declare `trusty-mcp-service` as a workspace dep and remove its own `teloxide` dep, or keep parallel deps for the transition? | Engineering | Parallel deps risk version skew; single dep preferred. |
| OQ-4 | What is the answer/action store TTL and capacity for v1? | Engineering | 15 min / 1 000 entries proposed; adjust based on observed session lengths. |
| OQ-5 | Should the gworkspace domain expose a `rpc.discover` `ServiceDescriptor` entry at the daemon layer (for unified `rpc.discover` across all trusty daemons)? | Engineering | `trusty_common::mcp::ServiceDescriptor` is the hook; evaluate in WI-1. |
| OQ-6 | How does the MCP-A `context` primitive interact with `trusty-mpm`'s existing per-workspace settings? | trusty-mpm owner | Needs alignment to avoid conflicting account selectors. |
| OQ-7 | Should `trusty-mcp-service` publish to crates.io, or remain workspace-internal only? | Bob | trusty-analyze is workspace-only; trusty-common publishes. Decide at WI-7. |

---

## 12. Phased Work Items

Work items map to a planned epic (tickets TBD). Each WI is independently mergeable.

### WI-1 — Framework & Scaffold (TBD)

**Scope:** crate skeleton, `Cargo.toml`, feature flags (`gworkspace`, `slack`, `telegram`,
`axum-server`), empty `Domain` trait, `DomainRegistry`, MCP-A primitive input/output types
(`primitives.rs`), `McpError`, `ToolResult`, `McpaErrorCode` enum, answer/action store stub,
`cargo check` passing across the workspace, line-cap check clean.

**Acceptance:** `cargo check -p trusty-mcp-service` passes; `cargo clippy -p trusty-mcp-service -- -D warnings` clean.

### WI-2 — Auth & HTTP Foundation (TBD)

**Scope:** `AuthProvider` trait, `AuthError`, two-tier credential storage, `mask()`,
`find_free_port()`, OAuth2 PKCE scaffolding (`oauth_pkce.rs`), HTTP retry layer, cursor
paginator, `RequestContext` + `task_local!`, `TokenCache`.

**Acceptance:** unit tests for credential resolution order, mask(), retry logic pass;
`cargo test -p trusty-mcp-service` (non-ignored) passes.

### WI-3 — `trusty-gworkspace` Domain (TBD)

**Scope:** all 7 sub-domains (Gmail/Calendar/Drive/Docs/Sheets/Slides/Tasks), typed input
structs, `schema()` implementation, `classify_query()` stub, MCP-A conformance fixtures
for gworkspace operations, `#[ignore]` live API integration tests.

**Acceptance:** conformance test suite passes against `~/Projects/mcp-a-protocol/examples/`
fixtures; `cargo test -p trusty-mcp-service --features gworkspace` (non-ignored) passes.

### WI-4 — `trusty-slack` Domain (TBD)

**Scope:** all 8 sub-domains (channels/messages/users/files/workspace/reminders/bookmarks/
scheduled), typed input structs, `schema()`, `classify_query()` stub, conformance fixtures,
`#[ignore]` live API integration tests.

**Acceptance:** same as WI-3 for `--features slack`.

### WI-5 — `trusty-telegram` Move (TBD)

**Scope:** copy `crates/trusty-mpm/src/telegram/` → `trusty_mcp_service::domains::telegram`;
update `trusty-mpm` to re-export from `trusty-mcp-service/telegram`; update `trusty-mpm`
`telegram` feature gate; delete the in-tree copy from `trusty-mpm`; verify alert-path
preservation contract (§5.5).

**Acceptance:** `cargo test -p trusty-mpm -- telegram` passes; `cargo test -p trusty-mcp-service --features telegram` passes; no behavioral delta in alert delivery.

### WI-6 — Consumer Integration (TBD)

**Scope:** `inject_trusty_mcp_service_mcp` in `trusty-mpm` settings; new plugin in
`trusty-agents/src/plugins/trusty_mcp_service.rs`; new tool provider in
`trusty-code/src/tools/mcp_service.rs`; all with graceful degradation when binary absent.

**Acceptance:** `cargo test -p trusty-mpm`, `cargo test -p trusty-agents`, `cargo test -p trusty-code` all pass; manual smoke test: `.mcp.json` wiring surfaces 7 MCP-A tools in a claude session.

### WI-7 — Packaging & Release (TBD)

**Scope:** `trusty-mcp-serviced` binary polish (clap CLI, `--help`, version flag); `launchd`
plist template; `cargo install` instructions in `docs/reference/running-mcp-servers.md`;
version bump; crates.io publication decision (OQ-7); CHANGELOG stub.

**Acceptance:** `cargo install --path crates/trusty-mcp-service --locked` installs both
binaries; `trusty-mcp-serviced --version` prints version; docs updated.

---

## 13. References

### MCP-A Protocol

- Spec and JSON Schemas: `~/Projects/mcp-a-protocol/` (v1.0.1-beta)
  - Schemas: `~/Projects/mcp-a-protocol/schemas/*.json`
  - Conformance fixtures: `~/Projects/mcp-a-protocol/examples/*.json`

### Prior-Art Python Projects

- Google Workspace MCP: `~/Projects/gworkspace-mcp/` — OAuth2 PKCE, multi-account,
  Gmail/Calendar/Drive/Docs/Sheets/Slides/Tasks tool implementations
- Slack MCP: `~/Projects/slack-mpm/` — bot+user token auth, channels/messages/users/files/
  workspace/reminders/bookmarks/scheduled tool implementations

### In-Tree Files Cited

| File | Relevance |
|------|-----------|
| `crates/trusty-common/src/mcp/mod.rs` | JSON-RPC framing, `run_stdio_loop`, `ServiceDescriptor` |
| `crates/trusty-common/src/mcp/daemon_bridge.rs` | `DaemonBridgeConfig`, `ensure_daemon_up` |
| `crates/trusty-common/src/mcp/openrpc.rs` | `OpenRpcBuilder` |
| `crates/trusty-common/src/mcp/service.rs` | MCP service primitives |
| `crates/trusty-analyze/src/mcp/stdio.rs` | `guard_response_size`, `TRUSTY_MCP_MAX_RESPONSE_BYTES` |
| `crates/trusty-mpm/src/core/session_launch/settings.rs` | `inject_trusty_memory_mcp`, `inject_trusty_search_mcp` — template for WI-6 |
| `crates/trusty-agents/src/plugins/trusty_memory.rs` | `StdioMcpClient::spawn`, plugin pattern |
| `crates/trusty-agents/src/plugins/mod.rs` | `PluginManager::init` |
| `crates/trusty-code/src/tools/` | Tool provider injection pattern |
| `crates/trusty-mpm/src/telegram/` | Source for WI-5 move |
| `docs/specs/telui-telegram-ui.md` (DOC-19) | TELUI UX contract (Telegram domain consumed-by) |
| `docs/specs/standalone-managed-trusty-mpm.md` (DOC-24) | MCP wiring model that WI-6 extends |
