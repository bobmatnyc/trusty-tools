---
spec_refs:
  - id: SPEC-REQ-01~draft
    path: docs/specs/DOC-43-requirements-standard.md
    anchor: SPEC-REQ-01~draft
---

# Requirements Index

This index catalogs all traceable requirements in the trusty-tools workspace, organized by domain and linked to their governing specifications (per DOC-43 — Requirements Standard).

**Conformance:** All listed requirements follow the EARS (Easy Approach to Requirements Syntax) patterns and are linked to one or more DOC-N specifications via frontmatter `spec_refs:`.

---

## trusty-code Requirements

**Governed by:** [DOC-39 — trusty-code Harness UI](../specs/trusty-code-harness-ui.md), [DOC-40 — Durable Background Agents](../specs/durable-background-agents.md)

### [CLAUDE.md and AGENTS.md Support (Non-Hierarchical Loading)](./trusty-code/CLAUDE-AGENTS-load.md)

- **REQ-TCUI-001** — Load CLAUDE.md from project root only (non-hierarchical)
- **REQ-TCUI-002** — Load AGENTS.md from project root only; precedence over defaults

### [Claude Code Plugins Support](./trusty-code/plugins-support.md)

- **REQ-TCUI-003** — Discover Claude Code plugins via trusty-code REST API
- **REQ-TCUI-004** — Load and expose plugin-provided capabilities in harness context

---

## trusty-mpm Requirements (Placeholder)

**Governed by:** [DOC-30 — Project Manager](../specs/DOC-30-project-manager-vision.md), [DOC-14 — Session Manager Agent](../specs/session-manager-agent.md), [DOC-26 — unified control plane](../specs/trusty-mpm-alpha-1-control-plane.md)

*(Stub — to be populated as trusty-mpm requirements are extracted from existing specs)*

---

## trusty-common Requirements (Placeholder)

**Governed by:** [DOC-38 — Spec-Linked Documentation](../specs/spec-linked-documentation.md), [DOC-15 — Intent Conformance](../specs/intent-conformance.md)

*(Stub — to be populated as trusty-common requirements are extracted from existing specs)*

---

## trusty-search Requirements (Placeholder)

**Governed by:** [DOC-37 — trusty-search Managed-Repo Awareness](../specs/trusty-search-managed-repo-awareness.md)

*(Stub — to be populated as trusty-search requirements are extracted from existing specs)*

---

## trusty-analyze Requirements (Placeholder)

**Governed by:** (Awaiting first spec)

*(Stub — to be populated once trusty-analyze specs are published)*

---

## Cross-Crate Requirements (Placeholder)

**Governed by:** Multiple specs (session attach/detach, SLD conformance, etc.)

*(Stub — to be populated for requirements spanning multiple crates)*

---

## How to Add a Requirement

1. **Check the standard:** Read [DOC-43 — Requirements Standard](../specs/DOC-43-requirements-standard.md), particularly §3 (EARS patterns) and §4 (requirement attributes).
2. **Pick a domain:** Place the requirement in the appropriate subdirectory (`trusty-code/`, `trusty-mpm/`, etc.). Create the directory if it doesn't exist.
3. **Write the requirement:** Follow the Markdown template in the standard; include frontmatter with `id`, `title`, `priority`, `status`, `owning_crate`, `verification_method`, and `spec_refs`.
4. **Use EARS:** State the requirement in one of the five EARS patterns (ubiquitous, event-driven, state-driven, unwanted-behavior, optional).
5. **Link the governing spec:** Add `spec_refs:` entries that point to the DOC-N and SPEC-ID that justify this requirement.
6. **Add backward link in the spec:** Edit the governing spec to add a `## Implementing requirements` section listing this REQ-ID.
7. **Update the index:** Add the requirement to this file under the appropriate domain.

---

## Organization Rationale

The tree mirrors the spec catalog structure (one directory per major component/domain) so that requirements and specs co-locate by concern. This makes it natural to:

- **Navigate:** Readers browse a domain (e.g., `trusty-code/`) and discover all requirements in that area.
- **Trace:** Each requirement links forward to its spec (`spec_refs:`); the spec links backward to the requirements it implements (`## Implementing requirements`).
- **Search:** Tools can grep for `REQ-<DOMAIN>-` to find all requirements in a domain or link to a spec by its SPEC-ID.

---

## Future Gates & Tooling

While this standard does not mandate enforcement (DOC-43 §1.3), conforming organizations MAY implement:

- `bash scripts/check_requirements.sh` — validates all `docs/requirements/**/*.md` files against the schema and checks spec links.
- PR automation — require that PRs referencing new requirements declare them in the body (e.g., `REQ-TCUI-001`).
- Traceability graphs — generate a bidirectional index linking specs ↔ requirements ↔ code locations.

These tools are outside the scope of this standard but are encouraged as adoption evolves.
