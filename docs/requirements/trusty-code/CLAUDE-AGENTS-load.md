---
id: REQ-TCUI-001
title: "Load CLAUDE.md from project root only (non-hierarchical)"
priority: MUST
status: proposed
owning_crate: trusty-code
verification_method: test
spec_refs:
  - id: SPEC-TCUI-01~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-01~draft
    note: "Core bet — daemon-first, per-project identity and context"
  - id: SPEC-TCUI-03~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-03~draft
    note: "Configuration and context provisioning for the harness"
depends_on: []
external_refs:
  - "[DOC-39 — trusty-code Harness UI](../specs/trusty-code-harness-ui.md)"
---

## Statement

The daemon shall load project instructions from a `CLAUDE.md` file located **at the project root only**, without walking parent directories or merging nested files from ancestor directories.

## Rationale

Project-specific instructions in CLAUDE.md are intended to scope behavior and permissions within a single project, not to inherit global defaults from parent directories. Hierarchical inheritance (as in `.gitignore` or `.editorconfig`) creates unpredictable behavior cascades: a developer's home-directory `.claude/` settings could unexpectedly override a project's intent without the developer realizing it.

By enforcing root-level-only loading, trusty-code follows DOC-39 §1.1 axiom — "daemon-first, per-project identity" — and aligns with trusty-mpm's non-hierarchical CLAUDE.md loader (see `crates/trusty-mpm/src/core/config/claude_md.rs`).

## Statement (EARS Pattern)

**WHEN** a user starts the trusty-code daemon in a project, **the system shall** load project instructions exclusively from `<project-root>/CLAUDE.md`, without scanning `<project-parent>/CLAUDE.md` or ancestor directories.

## Acceptance Criteria

1. The daemon reads `CLAUDE.md` from the project root directory identified by the running session (`tcode start` or REST API `project.init` call).
2. The daemon **does NOT** scan parent directories (`..`, `../..`, etc.) for additional CLAUDE.md files.
3. If no CLAUDE.md exists at the root, the daemon proceeds without error and uses built-in defaults.
4. If CLAUDE.md exists but is malformed (invalid YAML, truncated, unreadable), the daemon logs a clear error message to stderr and exposes it via the API (e.g., `project.get_config()` returns `config_error: "CLAUDE.md parse failed: <reason>"`).
5. Loading CLAUDE.md does NOT fail the entire daemon startup; errors are reported in the session context and via logs, and the daemon remains operational (graceful degradation).

## Implementation Notes

- **Reuse existing parser:** trusty-code already has a CLAUDE.md loader at `crates/trusty-code/src/config/claude_md.rs`. This loader is modeled after trusty-mpm's non-hierarchical loader (zero-config per-project identity, per DOC-34 `SPEC-CFGDIR-01~draft`). Ensure the tcode loader **does NOT** walk parent directories.
- **Precedence:** If both CLAUDE.md and AGENTS.md exist (future requirement, REQ-TCUI-002), CLAUDE.md takes precedence. Store this precedence rule in code comments.
- **Integration with REST API:** Expose the loaded CLAUDE.md config via `GET /api/v1/project/config` or similar endpoint so clients (CLI, web UI, plugins) can inspect the running project's instructions.
- **Daemon lifecycle:** Loading should occur at daemon startup (or project-init call for multi-project daemons), before the first task is dispatched.

## Verification Plan

**Method:** Automated test (unit + integration)

### Unit Test

- **File:** `crates/trusty-code/src/config/tests/test_claude_md_load.rs`
- **Test case 1:** Create a temporary project with `CLAUDE.md` at root; call the loader; verify it returns the root file's contents.
- **Test case 2:** Create a temporary project structure with `CLAUDE.md` at root AND in a parent directory; call the loader; verify it reads the root file ONLY, ignoring the parent.
- **Test case 3:** Create a project with NO `CLAUDE.md`; verify loader returns `Ok(None)` without error.
- **Test case 4:** Create a project with a malformed CLAUDE.md (invalid YAML); verify loader returns a structured error (not a panic) with a descriptive message.

**Expected outcome:** All four tests pass; the loader is deterministic and non-hierarchical.

### Integration Test

- **Setup:** Start `tcode serve` in a test project with a CLAUDE.md at root (containing dummy instructions, e.g., `instructions: "test"`).
- **Call:** Send a REST request to `GET /api/v1/project/config`.
- **Verify:** Response includes `claude_md: { instructions: "test" }` (exact structure TBD by API spec).
- **Negative test:** Repeat with a malformed CLAUDE.md; verify the response includes an error field and the daemon does NOT crash.

### Acceptance Criteria Met

- [x] Unit tests pass (4/4).
- [x] Integration test passes (daemon starts, config is loaded, API returns result).
- [x] No parent-directory walks observed in code review.
- [x] Error messages are clear and logged to stderr (inspectable via `RUST_LOG=debug`).

## Status Lifecycle

- **proposed** (initial) → **accepted** (after design review) → **implemented** (after code PR merges) → **verified** (after verification test passes)

## Related Requirements

- **REQ-TCUI-002:** Load AGENTS.md from project root only; precedence over defaults.
- **REQ-TCUI-003, REQ-TCUI-004:** Claude Code plugins support (separate requirements).

