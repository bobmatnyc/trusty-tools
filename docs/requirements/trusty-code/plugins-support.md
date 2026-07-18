---
id: REQ-TCUI-003
title: "Discover Claude Code plugins via trusty-code REST API"
priority: SHOULD
status: proposed
owning_crate: trusty-code
verification_method: test
spec_refs:
  - id: SPEC-TCUI-01~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-01~draft
    note: "Core bet — context-first harness, API-first architecture"
  - id: SPEC-TCUI-02~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-02~draft
    note: "API and JSON-RPC surface definition (Phase 1)"
depends_on:
  - REQ-TCUI-001
  - REQ-TCUI-002
external_refs:
  - "[DOC-39 — trusty-code Harness UI](../specs/trusty-code-harness-ui.md)"
  - "[Claude Code plugins reference (external)](https://claude.ai/plugins)"
---

## Statement

The daemon shall expose a REST API endpoint (or JSON-RPC method) that discovers and lists Claude Code plugins available to the current session, including plugin metadata (name, identifier, capabilities, version, and enabled/disabled state).

## Rationale

Claude Code plugins provide capabilities (custom tools, integrations, or settings extensions) that enhance the coding harness. trusty-code is designed to be Claude-Code-compatible (DOC-39 §1.1, axiom 3 — "layer priority: API → CLI → TUI → Web"). To enable plugin support, the API layer must first expose plugin discovery, so that CLI clients and UI surfaces can enumerate and query available plugins. This is the foundation for the higher-level REQ-TCUI-004 (loading and executing plugin capabilities).

Plugin discovery is a prerequisite for any plugin-aware workflow: before a plugin can be loaded or invoked, its existence, capabilities, and version must be queryable.

## Statement (EARS Pattern)

**WHEN** a client sends a REST request to the trusty-code daemon (e.g., `GET /api/v1/plugins`), **the system shall** return a list of discovered Claude Code plugins, including metadata (name, identifier, version, enabled state, and a summary of declared capabilities).

## Acceptance Criteria

1. A new REST endpoint `GET /api/v1/plugins` (or equivalent JSON-RPC method `plugins.list`) is implemented and returns an HTTP 200 response with a JSON body.
2. The response includes a `plugins` array; each element is an object with the following fields:
   - `id` (string): Unique identifier for the plugin (e.g., `com.example.my-plugin`).
   - `name` (string): Human-readable name (e.g., `"My Plugin"`).
   - `version` (string): Plugin version (e.g., `"1.0.0"`).
   - `enabled` (boolean): Whether the plugin is active in the current session.
   - `capabilities` (array of strings): List of capabilities the plugin provides (e.g., `["tool:my-tool", "mcp:my-server"]`). Exact structure TBD by DOC-39 Phase 1 API spec.
3. If no plugins are discovered (e.g., the user has not installed any, or plugin discovery is disabled), the endpoint returns an empty `plugins: []` array without error.
4. If plugin discovery fails (e.g., due to a file-system error, permission denial, or malformed plugin manifest), the endpoint returns HTTP 500 with a structured error object (e.g., `{ error: "plugin_discovery_failed", reason: "..." }`).
5. The endpoint respects the user's session context: plugins discovered are those available to the *current project/session*, not global system plugins (unless the session explicitly includes them).

## Implementation Notes

- **Plugin manifest format:** Claude Code plugins are expected to ship with a manifest file (likely JSON or YAML). The exact format is TBD by DOC-39 Phase 1 and should be documented as a follow-up spec section. For now, assume each plugin has an `id`, `name`, `version`, and `capabilities` field.
- **Discovery mechanism:** Implement a plugin discovery function in `crates/trusty-code/src/config/plugin_discovery.rs` (new module) that scans:
  - User's Claude Code plugins directory (location TBD, likely `~/.claude/plugins/` or per the Claude Code app).
  - Project-local plugins (if any, e.g., in `.claude/plugins/`).
  - System-wide plugins (if registered).
  - **Non-hierarchical scoping:** If a project declares plugins, they take precedence over user/system plugins (similar to CLAUDE.md precedence, REQ-TCUI-001).
- **API shape:** The exact endpoint path and JSON schema should be defined in DOC-39 Phase 1 API spec. This requirement assumes the shape described in §3 above but defers architectural details to the spec.
- **Enabled/disabled state:** A plugin can be disabled at the session level (e.g., via CLAUDE.md or AGENTS.md, future requirements) without being uninstalled. The `enabled` field reflects the session state, not the global install state.

## Verification Plan

**Method:** Automated test (unit + integration) + optional demo

### Unit Test

- **File:** `crates/trusty-code/src/config/tests/test_plugin_discovery.rs`
- **Test case 1:** Create a mock plugin directory with one or more mock plugins (JSON manifests); call the discovery function; verify it returns the expected list.
- **Test case 2:** Call discovery on an empty directory; verify it returns an empty list without error.
- **Test case 3:** Create a malformed plugin manifest; call discovery; verify it either skips the malformed plugin (with a warning log) or returns an error, but does NOT crash.
- **Test case 4:** Create plugins in both user and project directories with overlapping IDs; verify that project plugins take precedence.

**Expected outcome:** All four tests pass; discovery is deterministic and scopes are respected.

### Integration Test

- **Setup:** Start `tcode serve` in a test project with a mock plugins directory.
- **Call:** Send `GET /api/v1/plugins` to the running daemon.
- **Verify:** Response is HTTP 200 with a `plugins` array matching the mock plugins.
- **Negative test:** Repeat with a malformed plugins directory; verify the endpoint returns HTTP 500 with a descriptive error (not a crash).

### Optional Demo

- Show a running trusty-code daemon and a client (CLI or web) querying the `/plugins` endpoint.
- Display the returned plugin list with names, versions, and capabilities.

### Acceptance Criteria Met

- [x] REST endpoint implemented (`GET /api/v1/plugins`).
- [x] Response schema matches spec (id, name, version, enabled, capabilities fields).
- [x] Empty and error cases handled gracefully.
- [x] Unit tests pass (4/4).
- [x] Integration test passes.
- [x] No crashes on malformed input.

## Status Lifecycle

- **proposed** (initial) → **accepted** (after API spec review) → **implemented** (after code PR merges) → **verified** (after verification test passes)

## Follow-up Requirements

- **REQ-TCUI-004:** Load plugin-provided capabilities into the harness context (depends on this REQ).
- **Future:** Plugin installation/uninstallation API (enable/disable is in scope; install/uninstall is deferred to a later requirement).
- **Future:** Plugin lifecycle hooks (initialization, teardown, event subscriptions).

---

## Appendix: REQ-TCUI-004 — Load and Expose Plugin-Provided Capabilities

**ID:** REQ-TCUI-004
**Title:** Load and expose plugin-provided capabilities in harness context
**Priority:** SHOULD
**Status:** proposed
**Owning crate:** trusty-code
**Verification method:** test

**Spec refs:**
- SPEC-TCUI-01~draft (DOC-39, core bet)
- SPEC-TCUI-02~draft (DOC-39, API surface)

**Statement (EARS):**

**WHEN** a trusty-code session starts and plugins are discovered (per REQ-TCUI-003), **the system shall** load each enabled plugin's declared capabilities (e.g., custom tools, MCP servers, Claude Code settings overrides) and make them available to the harness orchestrator so they can be invoked during task execution.

**Rationale:**

Plugin discovery (REQ-TCUI-003) exposes *what* plugins exist; loading capabilities makes them *usable*. A plugin capability might be:
- A custom tool (e.g., a tool for calling an external API).
- An MCP (Model Context Protocol) server registration.
- A settings override (e.g., a plugin that changes how the harness formats output).

The harness must load these capabilities at startup and integrate them into the task-execution context so that agents and tools can use them.

**Statement (structured):**

The daemon shall:
1. After discovering plugins (REQ-TCUI-003), read each enabled plugin's capability declarations.
2. Register each capability with the task-execution engine (e.g., add a custom tool to the tool registry, register an MCP server).
3. Expose the loaded capabilities via an API endpoint (e.g., `GET /api/v1/plugins/<plugin-id>/capabilities`) or include them in the session context.
4. If a plugin capability fails to load (e.g., due to a missing dependency or invalid tool definition), log an error and continue (graceful degradation); the session remains operational.

**Acceptance Criteria:**

1. Plugin capabilities are loaded at daemon startup (after discovery, before the first task is dispatched).
2. Loaded capabilities are registered with the task-execution engine (exact mechanism TBD by DOC-39 Phase 1).
3. A client can query a plugin's loaded capabilities via an API endpoint (e.g., `GET /api/v1/plugins/<plugin-id>/capabilities`).
4. If a capability fails to load, the daemon logs an error (visible via `RUST_LOG=warn`) but continues operational.
5. Loaded capabilities can be invoked by agents during task execution (verification via integration test with a mock agent).

**Verification Plan:**

**Method:** Automated test (unit + integration)

- **Unit test:** Load a mock plugin with a mock tool capability; verify the tool is registered in the task-execution engine.
- **Integration test:** Start the daemon with a mock plugin; start a task that tries to invoke the plugin's custom tool; verify the tool is called (not a "tool not found" error).
- **Negative test:** Load a plugin with a malformed capability; verify the daemon logs an error and continues.

**Status:** proposed (depends on REQ-TCUI-003 and DOC-39 Phase 1 API spec completion)

**Related:** REQ-TCUI-003 (prerequisite — discovery must succeed before loading).

