# Service Layer Audit — Issue #484 Phase 1

> Audit of coded API integrations across the Trusty ecosystem, and the plan to
> consolidate them into a shared `tc-services` crate in `trusty-common`.

## Why this audit exists

API integrations (CTO SQLite DB, Google Workspace, Granola) are currently
reimplemented independently in every project that needs them. Each
reimplementation drifts: different error handling, different schema shapes,
different auth flows. This audit maps the current state, the duplication, and
the migration order so the consolidation can proceed in safe phases.

## 1. Current State — what lives where

| Integration | Location | Form | Notes |
|---|---|---|---|
| CTO ops SQLite DB | `trusty-common/crates/trusty-cto-db` | Rust crate (library) | Source of truth: `query_headcount`/`query_budget`/`query_risks`/`query_work_classification`, `tool_list_response()`, `handle_tool_call()`. |
| CTO DB tool wrapper | `open-mpm/src/tools/cto_db.rs` | open-mpm `ToolExecutor` impls | `CtoDbToolExecutor` — adapts `trusty-cto-db` to open-mpm's tool registry. open-mpm-specific. |
| MCP service tools | `open-mpm/src/tools/mcp_tools.rs`, `mcp_service_tools.rs` | open-mpm `ToolExecutor` impls | Bridge to MCP subprocesses (gworkspace-mcp etc.). Transport glue, not a service implementation. |
| Google Workspace bridge | `trusty-common/crates/trusty-gworkspace` | Rust crate | Exists as a crate; no `tc-services` wrapper yet. |
| Google Workspace OpenRPC spec | `trusty-common/skills/gworkspace-calendar.md` | Markdown spec only | Spec, no implementation behind it. |
| CTO DB OpenRPC spec | `trusty-common/skills/cto-db-openrpc.md` | Markdown spec only | Spec, no implementation behind it. |
| trusty-izzie services | `trusty-izzie` (separate repo) | Rust service impls | Duplicate implementations of the same integrations. |
| Python CTO bot services | `~/Duetto/cto/app/cto_bot/services/` | Python service impls | Independent Python reimplementation of CTO-domain integrations. |
| Granola API client | (none — only `grn_` key + `public-api.granola.ai/v1/` referenced) | — | No coded client anywhere yet; each consumer would hand-roll one. |

## 2. Duplication Map

| Integration | open-mpm | trusty-izzie | Python CTO bot | trusty-common |
|---|---|---|---|---|
| CTO DB queries | wrapper only (`cto_db.rs`) | duplicate impl | duplicate impl (Python) | `trusty-cto-db` (canonical) |
| Google Workspace | MCP-subprocess bridge | duplicate impl | duplicate impl (Python) | `trusty-gworkspace` crate |
| Granola | — | possible ad-hoc | possible ad-hoc | — |

Key observation: the CTO DB *engine* is already consolidated in
`trusty-cto-db`. What is NOT consolidated is the **service-layer adapter** —
the code that builds tool schemas and dispatches calls. open-mpm, trusty-izzie,
and the Python bot each re-derive that adapter. `tc-services` will own one
framework-agnostic adapter so Rust consumers stop duplicating it.

## 3. Phase 1 Plan — move `cto_db` to `tc-services` (THIS PHASE)

Deliverables:

1. This audit doc.
2. New `tc-services` crate scaffold in `trusty-common/crates/tc-services/`,
   added to the `trusty-common` workspace `members`.
3. `tc-services::cto_db` — a **framework-agnostic** service adapter migrated
   from open-mpm's `src/tools/cto_db.rs`. It must NOT depend on open-mpm's
   `ToolExecutor` trait (that would be a circular dependency: `tc-services`
   lives in `trusty-common`, which open-mpm depends on).

   Design (Anti-Corruption Layer + SOA):
   - `tc-services::cto_db::CtoDbService` holds a tool name + cached OpenAI
     function schema.
   - `CtoDbService::all()` returns one service per published tool name.
   - `CtoDbService::execute(args)` runs `trusty_cto_db::handle_tool_call`
     inside `spawn_blocking` and returns a `CtoDbOutcome` enum
     (`Ok(String)` / `Err(String)`) — a plain result type with no open-mpm
     dependency.
4. open-mpm's `src/tools/cto_db.rs` becomes a **thin wrapper**: a
   `CtoDbToolExecutor` newtype that holds a `tc_services::cto_db::CtoDbService`
   and implements open-mpm's `ToolExecutor` by delegating to it. `cto_db_tools()`
   keeps the same signature so `ctrl::mod.rs` registration is untouched.

Rationale for the split: open-mpm's `ToolExecutor` is a host-framework
concern. `tc-services` must stay host-agnostic so trusty-izzie (and a future
Python FFI consumer) can reuse it. The thin wrapper in open-mpm is the
Anti-Corruption Layer that translates the shared service into the host's tool
trait.

## 4. Phase 2 Plan — Granola + gworkspace (NOT in this phase)

1. `tc-services::granola` — a native Granola API client targeting
   `public-api.granola.ai/v1/` with `grn_`-prefixed key auth. First coded
   Granola client in the ecosystem; replaces ad-hoc per-consumer clients.
2. `tc-services::gworkspace` — a service-layer wrapper over the existing
   `trusty-gworkspace` crate, exposing the same `Service::all()` /
   `execute()` shape as `cto_db` so all integrations present a uniform
   surface.
3. Migrate trusty-izzie and (where feasible) the Python CTO bot onto
   `tc-services` so the duplicate implementations can be deleted.

## 5. Risk Assessment

| Risk | Severity | Mitigation |
|---|---|---|
| Circular dependency (`tc-services` → open-mpm trait) | High | `tc-services` exposes a plain `CtoDbOutcome` result type; open-mpm owns the `ToolExecutor` impl. No reverse dependency. |
| Behavior drift during the move | Medium | Service logic is copied verbatim from `trusty_cto_db` call sites; the four CTO DB tool tests are duplicated into `tc-services` and kept in open-mpm's wrapper test. |
| `rusqlite` native-lib link conflict | Medium | `tc-services` does NOT depend on `rusqlite` directly — only on `trusty-cto-db`, which already pins `rusqlite 0.39` to match `trusty-memory-core`. Single libsqlite3-sys version preserved. |
| Workspace build breakage in `trusty-common` | Medium | `tc-services` added to `members`; `cargo build` run in both repos as a verification gate. |
| `async-openai` version skew | Low | `tc-services` only emits JSON schemas (`serde_json::Value`); it does not need `async-openai`. The dependency is omitted from the scaffold to keep the crate light. open-mpm continues to own the async-openai bridge. |
| Migration order | Low | Phase 1 touches only CTO DB (already canonicalised in `trusty-cto-db`, lowest blast radius). Granola/gworkspace deferred to Phase 2. |

### Test coverage going in

- `trusty-cto-db`: existing crate tests cover query functions + dispatch.
- `open-mpm/src/tools/cto_db.rs`: four tests (`cto_db_tools_lists_four`,
  `schema_round_trips_for_each_tool`, `new_returns_none_for_unknown_name`,
  `execute_returns_recoverable_error_when_db_missing`).
- After Phase 1: equivalent tests live in `tc-services::cto_db`; open-mpm's
  wrapper keeps a reduced smoke test (`cto_db_tools_lists_four`) verifying the
  `ToolExecutor` adaptation still works.

## 6. trusty-common accessibility

`../trusty-common/` IS accessible from this machine and is a Cargo workspace
(`crates/` contains `trusty-common`, `trusty-cto-db`, `trusty-gworkspace`,
`trusty-mcp-core`, `trusty-rpc`, `trusty-tickets`, `trusty-symgraph`,
`trusty-embedder`). Phase 1 therefore creates `tc-services` directly in
`trusty-common/crates/` as designed — no temporary holding location needed.
