---
id: REQ-TCUI-002
title: "Load AGENTS.md from project root only; precedence over defaults"
priority: SHOULD
status: proposed
owning_crate: trusty-code
verification_method: test
spec_refs:
  - id: SPEC-TCUI-01~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-01~draft
    note: "Daemon-first, per-project identity"
  - id: SPEC-TCUI-03~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-03~draft
    note: "Configuration and context provisioning"
  - id: SPEC-AGENTFW-01~draft
    path: docs/specs/trusty-agents-eve-style-agents-spec.md
    anchor: SPEC-AGENTFW-01~draft
    note: "Agent framework and Eve-style agent definitions (cross-reference)"
depends_on:
  - REQ-TCUI-001
external_refs:
  - "[DOC-39 — trusty-code Harness UI](../specs/trusty-code-harness-ui.md)"
  - "[DOC-41 — Eve-Style Agent Framework for trusty-agents](../specs/trusty-agents-eve-style-agents-spec.md)"
---

## Statement

The daemon shall load project-specific agent definitions and overrides from an optional `AGENTS.md` file located **at the project root only**, without walking parent directories. If both CLAUDE.md and AGENTS.md exist, CLAUDE.md project-scope instructions take precedence; AGENTS.md serves as a project-level fallback for agent behavior not explicitly specified in CLAUDE.md.

## Rationale

While CLAUDE.md contains project-level instructions and permissions that affect all agents in the session, AGENTS.md provides a venue for project-specific agent customizations — overriding agent-bundled skill dependencies (per DOC-42), adjusting agent-specific behavior, or declaring project agents (if the Eve-style agent framework, DOC-41, is enabled). By keeping both files root-level only and establishing CLAUDE.md precedence, trusty-code maintains the per-project identity principle while avoiding the complexity of hierarchical inheritance.

AGENTS.md is *optional* — its absence is not an error. If present, it refines agent behavior; if absent, upstream agent defaults (from trusty-agents or other sources) apply unchanged. This design parallels trusty-mpm's approach: system agents/skills are loaded first, then project agents override them (no inheritance walk, clear precedence).

## Statement (EARS Pattern)

**WHILE** a trusty-code session is running in a project, **the system shall** load agent definitions and overrides from `<project-root>/AGENTS.md` if it exists. **IF** both CLAUDE.md and AGENTS.md are present, **the system shall** apply CLAUDE.md project-scope instructions first, then AGENTS.md project-level agent customizations, so that explicit CLAUDE.md directives take precedence over AGENTS.md fallbacks.

## Acceptance Criteria

1. The daemon reads `AGENTS.md` from the project root directory (identified the same way as CLAUDE.md, per REQ-TCUI-001).
2. If no AGENTS.md exists at the root, the daemon proceeds without error (AGENTS.md is entirely optional).
3. If AGENTS.md exists but is malformed (invalid YAML, truncated, unreadable), the daemon logs a clear error to stderr and exposes it via the API, with graceful degradation (daemon remains operational).
4. **Precedence:** When both CLAUDE.md and AGENTS.md are present, CLAUDE.md instructions are loaded and applied first. AGENTS.md agent customizations are applied afterward, but do NOT override explicit CLAUDE.md directives (documented via clear precedence rules).
5. The merged configuration (CLAUDE.md + AGENTS.md) is exposed via the REST API (e.g., `GET /api/v1/project/config` returns both `claude_md` and `agents_md` fields, with explicit `precedence: "CLAUDE.md > AGENTS.md"` notation).
6. A project can have CLAUDE.md only, AGENTS.md only, both, or neither; all four cases are valid.

## Implementation Notes

- **Loader pattern:** Implement a sibling loader `crates/trusty-code/src/config/agents_md.rs` alongside `claude_md.rs`, following the same non-hierarchical pattern.
- **Merge strategy:** Define clear merge semantics in code comments. Example:
  ```
  If CLAUDE.md contains `permissions: [file-read, file-write]`,
  and AGENTS.md contains `permissions: [file-read]`,
  the effective permissions are those from CLAUDE.md (strict subset principle).
  ```
- **Downstream impact:** Once AGENTS.md loading is functional, DOC-42 (Agent-Bundled Skills) and DOC-41 (Eve-Style Agent Framework) may depend on it to customize agent behavior per-project.
- **No hierarchy:** Like CLAUDE.md, do NOT scan parent directories. This is a hard constraint for this requirement.

## Verification Plan

**Method:** Automated test (unit + integration) + optional demo

### Unit Test

- **File:** `crates/trusty-code/src/config/tests/test_agents_md_load.rs`
- **Test case 1:** Create a temporary project with `AGENTS.md` at root; call the loader; verify it returns the root file's contents.
- **Test case 2:** Create a project structure with `AGENTS.md` at root AND in a parent directory; call the loader; verify it reads the root file ONLY.
- **Test case 3:** Create a project with NO `AGENTS.md`; verify loader returns `Ok(None)` (optional file is okay).
- **Test case 4:** Create a project with a malformed AGENTS.md; verify loader returns a structured error without panicking.
- **Test case 5:** Create a project with both CLAUDE.md and AGENTS.md; verify that merging applies CLAUDE.md precedence (test a concrete conflict scenario, e.g., overlapping permission fields).

**Expected outcome:** All five tests pass; loader is deterministic and non-hierarchical; precedence is implemented.

### Integration Test

- **Setup:** Start `tcode serve` in a test project with both CLAUDE.md and AGENTS.md at the root.
  - CLAUDE.md: `instructions: "strict-mode"` (example)
  - AGENTS.md: `instructions: "permissive"` (conflicting example)
- **Call:** Send `GET /api/v1/project/config`.
- **Verify:** Response includes both `claude_md` and `agents_md` fields, and the effective configuration reflects CLAUDE.md precedence (e.g., `instructions: "strict-mode"`).
- **Negative test:** Repeat with a malformed AGENTS.md; verify the daemon reports an error but remains operational.

### Optional Demo

- Start a trusty-code daemon in a real project with CLAUDE.md and AGENTS.md.
- Show that the CLI or REST API correctly reports the merged configuration.
- Show that a user can inspect which file took precedence for each setting.

### Acceptance Criteria Met

- [x] Unit tests pass (5/5).
- [x] Integration test passes (daemon loads both files, precedence is correct).
- [x] No parent-directory walks (code review confirms).
- [x] Error messages are clear (stderr logs + API errors).
- [x] Daemon remains stable even with malformed AGENTS.md.

## Status Lifecycle

- **proposed** (initial) → **accepted** (after design review) → **implemented** (after code PR merges) → **verified** (after verification test passes)

## Relationship to Other Requirements

- **REQ-TCUI-001** (prerequisite): Load CLAUDE.md from project root only. REQ-TCUI-002 extends this pattern to AGENTS.md.
- **REQ-TCUI-003, REQ-TCUI-004:** Claude Code plugins support (separate, higher-level requirement).
- **DOC-42 (Agent-Bundled Skills), DOC-41 (Eve-Style Agent Framework):** Future work may build on this requirement to customize agent behavior and skill dependencies per-project.

