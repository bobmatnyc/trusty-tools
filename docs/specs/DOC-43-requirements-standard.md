---
spec_refs:
  - id: SPEC-SLD-01~draft
    path: docs/specs/spec-linked-documentation.md
    anchor: SPEC-SLD-01~draft
  - id: SPEC-REQ-01~draft
    path: docs/specs/DOC-43-requirements-standard.md
    anchor: SPEC-REQ-01~draft
---

# DOC-43 — Requirements Standard: A Traceable, Verifiable, Spec-Linked Requirement Authoring Standard

**Status:** Draft
**Subsystem:** documentation standard — requirements engineering (language-agnostic, spec-driven)
**Owner:** Engineering (trusty-tools platform)
**Last-updated:** 2026-07-18
**Spec ID:** `SPEC-REQ-01~draft` … `SPEC-REQ-05~draft` (DOC-43)
**Builds on:**
- [DOC-38 — Spec-Linked Documentation (SLD)](./spec-linked-documentation.md) — requirements documents link to governing specs via frontmatter and inline references, using the same SLD triple `(spec-ID, path, anchor)`.
- [DOC-15 — Intent / Method Conformance](./intent-conformance.md) §6 — the conformance resolver pattern, which requirements inherit for traceability.
- [ISO/IEC/IEEE 29148:2018](https://en.wikipedia.org/wiki/IEEE_29148) — Systems and software engineering — Life cycle processes — Requirements engineering; defines characteristics of good requirements (necessity, unambiguity, verifiability, feasibility, singularity, completeness, consistency, traceability).
- [EARS (Easy Approach to Requirements Syntax)](https://www.incose.org/products-and-publications/systems-engineering-handbook) — a lightweight, pattern-based syntax for natural-language requirements that structures statements into predictable, machine-parseable forms.

**Cross-ref:** `docs/requirements/` — the living repository of authored requirements, organized by domain and linked to their governing specs via `spec_refs:` frontmatter.

> **Scope note.** This is a **pure, implementation-neutral requirements standard** — a set of conventions for authoring, organizing, and tracing requirements across projects in any language or domain. It does **not** introduce a requirements-management tool, a database, a CI gate, or enforcement machinery. A conforming author writes requirements following the patterns in §3–§5; a conforming reader parses them via SLD and cross-links; a conforming organization uses the patterns to organize and reference requirements, but the *implementation* of gates, versioning, and lifecycle tracking is outside this standard. The PR carrying this doc opens **no** code changes — only documentation and the seed requirements tree (§6).

---

## 0. Terminology

| Term | Meaning |
|---|---|
| **Requirement (REQ)** | A single, testable statement of what a system must, should, or may do. It is atomic (one behavior per REQ), verifiable (testable via inspection, analysis, demonstration, or test), and traceable (linked to its governing spec and implementation). |
| **REQ-ID** | A unique, stable identifier for a requirement, formatted as `REQ-<DOMAIN>-<NNN>` (e.g., `REQ-TCUI-001`). The ID is never reused, even if the requirement is superseded or deleted. |
| **Requirement attribute** | A structured field on a REQ: id, title, statement (in EARS syntax), rationale, priority (MUST/SHOULD/MAY per RFC 2119), verification method, status (proposed/accepted/implemented/verified), owning component/crate. |
| **EARS (Easy Approach to Requirements Syntax)** | A pattern-based approach to structuring natural-language requirements into 5 recognizable forms — ubiquitous, event-driven, state-driven, unwanted-behavior, optional — so that a reader can quickly parse the subject, trigger, condition, and expected behavior. |
| **Specification** | A behavior-contract document (a DOC-N spec, e.g., DOC-39 trusty-code Harness UI). A requirement links to one or more spec sections that *govern* it, so a resolver can trace the requirement back to the spec that motivates it. |
| **Traceability** | The ability to trace a requirement to its governing spec (forward link), and conversely to trace a spec section to the requirements that implement it and the code that fulfills those requirements (backward link). Traceability is established via SLD (frontmatter `spec_refs:` in the requirement document). |
| **Verification method** | The strategy by which a requirement is validated: test (automated unit/integration test), inspection (code review, static analysis), demonstration (live manual walkthrough), or analysis (first-principles reasoning or modeling). |
| **Priority** | The obligation level of a requirement per RFC 2119: **MUST** (mandatory, system incomplete without it), **SHOULD** (highly desirable, affects quality/usability), **MAY** (optional, nice-to-have, does not block release). |
| **Status** | The lifecycle state of a requirement: **proposed** (not yet reviewed or accepted), **accepted** (reviewed and deemed feasible), **implemented** (code delivered), **verified** (verification test passed). |

---

## 1. Purpose & Scope {#SPEC-REQ-01~draft}

**ID:** SPEC-REQ-01~draft
**Status:** Draft

### 1.1 What this standard establishes

1. **Requirements as a first-class engineering artifact**, peer to specs and code — a way to state *what* the system must do without prescribing *how*, and to link that statement to the spec that justifies it.
2. **A language-agnostic authoring standard** — the same patterns work for REST APIs, daemon protocols, CLI behavior, UI surfaces, library ABIs, and data-flow requirements. Requirements are domain-neutral; only the vocabulary changes.
3. **Tractable traceability** — requirements link to specs via SLD (§2), code links to requirements via comments/docstrings, and readers can walk the chain: spec ← requirement ← implementation.
4. **Verifiability and acceptability criteria** — every requirement declares *how* it will be verified (test, inspection, demonstration, analysis) so a reviewer can assess feasibility upfront and a validator can confirm completion.

### 1.2 Goals

- **G1 — Lightweight, not bureaucratic.** Authoring a requirement takes 5 minutes, not 5 days. Templates are minimal; prose is natural language with structure, not machine-generated forms.
- **G2 — Usable in sprints.** Requirements capture design intent at the start of a feature (from a spec or an issue); implementation PR links them; a post-implementation pass confirms verification. The cycle is fast.
- **G3 — Spec-driven, not speculative.** A requirement MUST link to a governing spec section; it never floats unmoored. If no spec exists, the requirement calls out that a follow-up spec is needed.
- **G4 — Bidirectional linking.** A requirement links forward to its spec (via `spec_refs:`); the spec links backward to requirement IDs (in its body, after a `## Implementing requirements` section). This round-trip is human-readable and machine-checkable.
- **G5 — Agnostic to organization.** The standard does not mandate a workflow engine, a database, or CI gates. Organizations embed these patterns into *their* processes (e.g., a Makefile gate that runs `check_requirements.sh`, a GitHub Action that scans PR bodies for requirement references, a doc-site generator that builds traceability graphs). The standard constrains the *format*, not the *implementation*.

### 1.3 Non-goals

Explicitly OUT OF SCOPE for this standard:

- **No requirements-management database.** Requirements are stored as Markdown documents in `docs/requirements/` (or any convention the organization chooses), not in a database or spreadsheet.
- **No CI/CD gate on requirement coverage.** The standard does *not* enforce that all implemented code is traced back to a requirement, nor does it measure coverage. Such gates are organization-specific and tracked separately (e.g., an internal CI job that parses SPEC-REQ IDs from code).
- **No versioning or lifecycle automation.** A requirement's status progresses manually via PR review and verification comments; the standard does not automate state transitions.
- **No per-requirement voting or consensus model.** The standard does not address *how* a requirement is accepted (e.g., who votes, how many reviewers). That is a project governance decision, not a requirement-authoring standard.
- **No conflation with user stories or product tickets.** A requirement states *what*, a story describes *why for whom*; a ticket tracks *work*. All three are useful; this standard addresses only the first.

---

## 2. Spec-Linked Traceability {#SPEC-REQ-02~draft}

**ID:** SPEC-REQ-02~draft
**Status:** Draft

Every requirement MUST declare which spec section(s) govern it, using the SLD frontmatter form (DOC-38 §2.5). A requirement without a spec link is **orphaned** and MUST be rejected at review time.

### 2.1 Frontmatter schema for requirements

```yaml
---
spec_refs:
  - id: SPEC-TCUI-01~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-01~draft
    note: "Core bet — context-first harness UI"
---
```

This declares: "This requirement is governed by SPEC-TCUI-01~draft (DOC-39, trusty-code Harness UI). If the spec changes, this requirement may need review."

### 2.2 Multiple governing specs

A requirement MAY link to multiple specs if it spans concerns. Example:

```yaml
spec_refs:
  - id: SPEC-TCUI-05~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-05~draft
  - id: SPEC-BGATTACH-02~draft
    path: docs/specs/durable-background-agents.md
    anchor: SPEC-BGATTACH-02~draft
    note: "Session attach/detach semantics"
```

### 2.3 Backward links in specs

Corresponding, a spec MUST carry a `## Implementing requirements` section (if requirements exist for it) that lists the REQ-IDs that implement that spec section. Example:

```markdown
### §5.2 Task Context Bundle {#SPEC-TCUI-05~draft}

**ID:** SPEC-TCUI-05~draft
**Status:** Draft

[Spec body here...]

#### Implementing requirements

- REQ-TCUI-005 — Task context NDJSON schema shall include search hit provenance
- REQ-TCUI-006 — Task context NDJSON schema shall include memory recall ranking and score
```

This allows readers to navigate spec ← → requirement bidirectionally.

---

## 3. EARS: Structured Requirement Phrasing {#SPEC-REQ-03~draft}

**ID:** SPEC-REQ-03~draft
**Status:** Draft

The **EARS (Easy Approach to Requirements Syntax)** method gives requirements structure without sacrificing readability. Every requirement MUST fit one of five patterns. This makes parsing by a reader or a tool deterministic: scan the opening phrase, and the subject, condition, and expected behavior are predictable.

### 3.1 The five EARS patterns

#### 1. Ubiquitous — "The <system> shall <response>"

**When to use:** A behavior that always holds, with no trigger or condition.

**Pattern:** `The <system> shall <action> <object> <qualifier>.`

**Examples:**
- ✓ "The daemon shall listen on HTTP port 8080 and accept JSON-RPC requests."
- ✓ "The CLI shall parse command-line arguments according to POSIX conventions."
- ✓ "The API shall return responses in UTF-8 encoding."

**Rationale:** Ubiquitous statements anchor the baseline behavior. Use them for default modes, initialization, and protocol-level constants.

---

#### 2. Event-Driven — "WHEN <trigger>, the <system> shall <response>"

**When to use:** A behavior triggered by an external stimulus (user action, network event, time-based wake, filesystem change).

**Pattern:** `WHEN <trigger>, the <system> shall <action> <object> <qualifier>.`

**Examples:**
- ✓ "WHEN a user runs `tcode start`, the daemon shall initialize project context and enter listen mode."
- ✓ "WHEN the task executor completes a subtask, the harness shall emit a task.completed event with the result."
- ✓ "WHEN a file is edited and saved, the indexer shall queue a reindex pass within 5 seconds."

**Rationale:** Event-driven requirements capture stimulus–response pairs. Use them for user actions, network transitions, and file-system watches.

---

#### 3. State-Driven — "WHILE <state>, the <system> shall <response>"

**When to use:** A behavior that holds continuously as long as a condition or state is true.

**Pattern:** `WHILE <state>, the <system> shall <action> <object> <qualifier>.`

**Examples:**
- ✓ "WHILE a task is running, the CLI shall stream progress events to stdout in real time."
- ✓ "WHILE the daemon is in shutdown mode, the API shall accept no new task requests."
- ✓ "WHILE the user is authenticated, the daemon shall cache the auth token and reuse it for subsequent requests."

**Rationale:** State-driven requirements capture continuous invariants. Use them for resource management, mode-dependent behavior, and lifecycle guards.

---

#### 4. Unwanted-Behavior — "IF <condition>, the <system> shall NOT <action>" or error handling

**When to use:** A failure mode, constraint violation, or forbidden action that the system must prevent or handle.

**Pattern:** `IF <condition>, the <system> shall <reject-action / error-response> <qualifier>.`

**Examples:**
- ✓ "IF a task tries to access a file outside the project root, the system shall deny the access and return error code 403 (Forbidden)."
- ✓ "IF the daemon runs out of memory, it shall gracefully shut down and log the cause to stderr."
- ✓ "IF a requirement is submitted without a governing spec link, the CI gate shall reject the PR."

**Rationale:** Unwanted-behavior requirements are defensive; they prevent bad states and specify recovery. Use them for input validation, resource limits, and security boundaries.

---

#### 5. Optional — "WHERE <feature> is enabled, the <system> shall <action>"

**When to use:** A behavior that applies only when a feature flag, configuration option, or architectural component is present.

**Pattern:** `WHERE <feature-or-condition> is enabled, the <system> shall <action> <object> <qualifier>.`

**Examples:**
- ✓ "WHERE the `plugins:` field is present in CLAUDE.md, tcode shall discover and load Claude Code plugins at startup."
- ✓ "WHERE the `TRUSTY_DEEP_ANALYSIS` environment variable is set, the harness shall invoke the deep-analysis pass before finalizing the solution."
- ✓ "WHERE a requirement links to multiple specs, the SLD resolver shall report all cross-links."

**Rationale:** Optional requirements capture conditional or feature-gated behavior. Use them for configuration-driven features, A/B tests, and graceful degradation.

---

### 3.2 Checklist: Is this a good EARS statement?

- [ ] **One subject.** Does it name exactly one system, component, or role? (Avoid "the system and the user shall…")
- [ ] **Clear trigger or condition.** Is the stimulus, state, or feature explicit? (Avoid vague "when needed" or "as appropriate")
- [ ] **Observable action.** Is the expected behavior concrete and measurable? (Avoid "be robust", "handle well", or "efficiently")
- [ ] **Testable success criterion.** Could a reviewer verify this without reading the implementation? (Avoid "the system shall work", "it shall be fast")
- [ ] **Single responsibility.** Does this REQ state one behavior, not two? (Split "the system shall X and Y" into two requirements)

---

## 4. Requirement Attributes & Metadata {#SPEC-REQ-04~draft}

**ID:** SPEC-REQ-04~draft
**Status:** Draft

Every requirement MUST carry the following attributes in a Markdown document. The format is a structured YAML-like block at the top (frontmatter), followed by Markdown body sections.

### 4.1 Frontmatter schema

```yaml
---
id: REQ-TCUI-001
title: "Session context — NDJSON event schema includes memory hits"
priority: MUST
status: proposed
owning_crate: trusty-code
verification_method: test
spec_refs:
  - id: SPEC-TCUI-05~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-05~draft
---
```

**Required fields:**

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Unique REQ-ID (e.g., `REQ-TCUI-001`). MUST be `REQ-<DOMAIN>-<NNN>` with zero-padded 3-digit counter. Never reuse or delete a REQ-ID. |
| `title` | string | One-sentence summary of what is required (< 80 chars). |
| `priority` | enum | **MUST** (required for completeness), **SHOULD** (high priority, affects quality), **MAY** (optional, nice-to-have). Use RFC 2119 semantics. |
| `status` | enum | **proposed** (new, awaiting review), **accepted** (reviewed, feasible), **implemented** (code delivered), **verified** (verification passed). |
| `owning_crate` | string | The Cargo crate / component responsible for this REQ (e.g., `trusty-code`, `trusty-mpm`, `trusty-common`). |
| `verification_method` | enum | **test** (automated test), **inspection** (code review / static analysis), **demonstration** (live walkthrough), **analysis** (reasoning / first-principles). |
| `spec_refs` | array | One or more spec sections governing this REQ (SLD frontmatter form, DOC-38 §2.5). MUST NOT be empty. |

**Optional fields:**

| Field | Type | Description |
|-------|------|-------------|
| `depends_on` | array | Other REQ-IDs this requirement depends on (e.g., `[REQ-TCUI-001, REQ-TCUI-002]`). Useful for sequencing. |
| `supersedes` | array | REQ-IDs this requirement replaces (e.g., `[REQ-OLD-001]`). Use when refining or replacing a requirement. |
| `external_refs` | array | Links to issues, PRs, or design docs outside the requirement tree. |

### 4.2 Markdown body sections

After the frontmatter, every requirement document MUST have the following sections:

#### Statement

The requirement in EARS form (one of the five patterns). Exactly one short paragraph. Example:

```markdown
## Statement

WHEN a user runs `tcode start` in a project with a CLAUDE.md file, 
the daemon shall load project instructions from the root CLAUDE.md non-hierarchically 
(root-level only, not walking parent directories or merging nested files).
```

#### Rationale

Why this requirement exists: what problem does it solve? What spec section motivates it? 2–3 sentences.

```markdown
## Rationale

Project instructions in CLAUDE.md are often project-specific and should not be 
inherited from parent directories (unlike gitignore or Editor configs). 
Requiring root-level-only loading prevents unexpected behavior cascades and 
simplifies the precedence model. (See DOC-39 §1, axiom 1 — daemon-first, 
per-project identity.)
```

#### Acceptance Criteria

Concrete, testable success conditions. A bulleted list, one per criterion.

```markdown
## Acceptance Criteria

- The daemon reads CLAUDE.md from the project root only; does NOT scan `..` or ancestor directories.
- If both CLAUDE.md and AGENTS.md exist, the precedence is: CLAUDE.md (project-specific instructions) takes priority over AGENTS.md (agent-provided fallback).
- If neither file exists, the daemon proceeds with no error.
- Loading errors (e.g., malformed YAML, permission denied) are logged to stderr and reported via the API with a clear error message.
```

#### Implementation Notes (optional)

Hints, design notes, or tradeoffs for the implementer. Should NOT prescribe the implementation, only flag concerns or clarifications.

```markdown
## Implementation Notes

- The CLAUDE.md parser already exists in `crates/trusty-code/src/config/claude_md.rs` 
  and reuses trusty-mpm's loader logic (non-hierarchical). Reuse that code path.
- If AGENTS.md is added, it will live in the same crate and follow the same pattern.
```

#### Verification Plan

How will this requirement be verified? Outline the test case(s), inspection criteria, or demo scenario.

```markdown
## Verification Plan

**Method:** Automated test (unit + integration)

1. **Unit test:** Create a temporary project with CLAUDE.md at root; verify daemon loads it and NOT a parent-directory CLAUDE.md.
2. **Integration test:** Start daemon with a project that has both CLAUDE.md and AGENTS.md; verify precedence via an API call to `project.get_config()`.
3. **Negative test:** Verify that a malformed CLAUDE.md produces a clear error message (not a panic or silent failure).

**Acceptance:** All tests pass; error messages are logged to the daemon log and exposed via the API error field.
```

---

## 5. Requirement Characteristics & Quality Checks {#SPEC-REQ-05~draft}

**ID:** SPEC-REQ-05~draft
**Status:** Draft

Good requirements exhibit these characteristics (derived from IEEE 29148):

| Characteristic | Meaning | How to check |
|---|---|---|
| **Necessary** | The requirement addresses a real gap or implements a spec section. | Is there a `spec_refs:` entry? Does the rationale link to a real problem? |
| **Unambiguous** | A reader understands the behavior the same way every time. | Do acceptance criteria eliminate alternate interpretations? Are terms defined? Is the EARS pattern used? |
| **Verifiable** | The requirement can be proven true or false via test, inspection, demo, or analysis. | Does the verification plan include a concrete test case? Is the success criterion observable? |
| **Feasible** | The requirement is technically achievable with available resources and technology. | Is the owning crate realistic? Are dependencies listed? Is the scope bounded? |
| **Singular** | One requirement = one behavior; no "and" conjunctions that smuggle two requirements into one. | Can this REQ be split into two independent statements? |
| **Complete** | The requirement stands alone; a reader does not need external context. | Does it define terms? Does it state pre/postconditions? |
| **Consistent** | No conflict with other requirements in the same domain. | Does it contradict a sibling REQ? Are priorities aligned with the spec? |
| **Traceable** | The requirement links to a spec (forward) and is referenced by the spec (backward); code links to the REQ via comments. | Is `spec_refs:` present? Does the spec list this REQ-ID in its `## Implementing requirements` section? |

### 5.1 Pre-submission checklist

Before opening a PR with new requirements, verify:

- [ ] **REQ-ID is unique** in the domain (use grep: `grep -r "^id: REQ-<DOMAIN>" docs/requirements/`).
- [ ] **Spec link is valid** — the `spec_refs:` path and anchor exist and match the spec's declared ID.
- [ ] **EARS pattern is used** — the statement starts with "The", "WHEN", "WHILE", "IF", or "WHERE", and follows the pattern grammar.
- [ ] **Statement is singular** — could not be split into two independent statements.
- [ ] **Acceptance criteria are testable** — each bullet can be verified by an automated test, inspection, or demo.
- [ ] **No contradictions** with sibling requirements or the spec's own statements.
- [ ] **Rationale explains the "why"** — not just a restatement of the statement.
- [ ] **Verification plan is concrete** — names test files, acceptance conditions, or inspection criteria; not vague ("ensure it works").

---

## 6. The Requirements Tree: Organization & Discovery {#SPEC-REQ-06~draft}

**ID:** SPEC-REQ-06~draft
**Status:** Draft

Requirements are organized in `docs/requirements/` with an **index** and **domain-scoped subdirectories**. This layout mirrors the spec catalog structure (DOC-38 §4) and makes requirements discoverable:

```
docs/requirements/
├── README.md                 # Index: list all domains, link to REQ-* files
├── trusty-code/              # Domain: trusty-code (harness UI, config, REST API)
│   ├── CLAUDE-AGENTS-load.md # REQ-TCUI-001, REQ-TCUI-002: CLAUDE.md/AGENTS.md support
│   ├── plugins-support.md    # REQ-TCUI-003: Claude Code plugins discovery & loading
│   └── ...
├── trusty-mpm/               # Domain: trusty-mpm (session, PM, agent dispatch)
│   ├── session-attach.md
│   └── ...
├── trusty-common/            # Domain: trusty-common (shared, library, intent_source)
│   └── ...
└── cross-crate/              # Domain: cross-crate features (multiple owning crates)
    └── sld-conformance.md    # REQ-XCAT-001: SLD resolution and resolver conformance
```

Each domain directory contains one or more `<feature>.md` files, each holding a single requirement in the format of §4. The index lists every file and links to it.

### 6.1 Index structure

The `docs/requirements/README.md` index MUST:

- [ ] List every requirement domain (trusty-code, trusty-mpm, trusty-common, trusty-search, trusty-analyze, cross-crate).
- [ ] Under each domain, list every `<feature>.md` file with a one-line description.
- [ ] Provide relative links to each file.
- [ ] Link to the governing spec (e.g., "Governed by DOC-39").

**Example:**

```markdown
# Requirements Index

## trusty-code (Governed by DOC-39 — trusty-code Harness UI, DOC-40 — Durable Background Agents)

- [CLAUDE.md and AGENTS.md support (non-hierarchical)](./trusty-code/CLAUDE-AGENTS-load.md)
  - REQ-TCUI-001: Load CLAUDE.md from project root only
  - REQ-TCUI-002: Load AGENTS.md from project root only
  
- [Claude Code plugins support](./trusty-code/plugins-support.md)
  - REQ-TCUI-003: Discover Claude Code plugins via API
  - REQ-TCUI-004: Load plugin-provided capabilities
```

---

## 7. Conformance: The `check_requirements.sh` gate (informative)

While this standard does **not** mandate CI gates or enforcement tooling (§1.3), a conforming organization MAY implement a `check_requirements.sh` script (similar to DOC-38's `check_sld.sh`) that:

1. **Scans all `docs/requirements/**/*.md` files** for requirements with frontmatter.
2. **Validates frontmatter schema** — all required fields present, values in allowed enums.
3. **Checks spec links** — confirms `spec_refs:` paths and anchors resolve in `docs/specs/`.
4. **Confirms uniqueness** — no duplicate REQ-IDs across the tree.
5. **Verifies backward links** — checks that each spec with a `## Implementing requirements` section lists valid REQ-IDs.
6. **Reports violations** — lists schema errors, broken links, duplicates.

Such a gate would NOT enforce coverage (whether all code is traced to a requirement), as that is beyond the standard's scope.

---

## 8. Examples: Two Seed Requirements for trusty-code {#SPEC-REQ-07~draft}

**ID:** SPEC-REQ-07~draft
**Status:** Draft

See `docs/requirements/trusty-code/CLAUDE-AGENTS-load.md` and `docs/requirements/trusty-code/plugins-support.md` for concrete requirement documents following the patterns above.

---

## 9. Migration & Adoption Path (informative)

- **Immediate (this PR):** Publish the standard and the seed requirements tree; start authoring new requirements in this format.
- **Phase 1 (next sprint):** Retrospectively extract requirements from DOC-39 (trusty-code Harness UI) and DOC-40 (Durable Background Agents) into dedicated REQ-* documents; add backward links to those specs.
- **Phase 2 (ongoing):** As new PRs land specs, require a follow-up requirements document (or a placeholder) at review time.
- **Phase 3 (optional):** Implement a `check_requirements.sh` gate and add it to CI; link requirements in PR bodies.

The standard itself changes only in minor ways (new sections, clarifications) — the core EARS patterns and traceability form (§2–§5) are stable.

