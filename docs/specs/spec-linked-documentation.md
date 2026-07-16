---
spec_refs:
  - id: SPEC-CONFORMANCE-03~draft
    path: docs/specs/intent-conformance.md
    anchor: SPEC-CONFORMANCE-03~draft
---

# DOC-38 — Spec-Linked Documentation (SLD): A Language-Agnostic Source↔Spec Reference Standard

**Status:** Draft
**Subsystem:** documentation standard — repository/language-agnostic (informative reference implementation: trusty-common `intent_source`, Annex A)
**Owner:** Engineering (trusty-tools platform)
**Last-updated:** 2026-07-16
**Spec ID:** `SPEC-SLD-01~draft` … `SPEC-SLD-03~draft` (DOC-38)
**Epic:** epic: spec-linked documentation, first-class & language-agnostic (umbrella issue to be linked by the PM)
**Builds on:**
- The SLD prose and spec-catalog conventions in [`docs/specs/README.md`](./README.md)
  (DOC-N numbering, `SPEC-{SUBSYSTEM}-{NN}~{rev}` grammar, `{#SPEC-…}` anchors,
  Draft→Accepted→Superseded lifecycle, the Rust `# Spec References` rustdoc form).
- DOC-15 [Intent / Method Conformance](./intent-conformance.md) §6
  (`SPEC-CONFORMANCE-03~draft`) — a **conforming resolver** (trusty-common's
  `intent_source`, informative Annex A) *reads* declared SLD links to resolve a
  changed file back to its governing spec section.
**Cross-ref:** the `spec-authoring` / `spec-linked-docs` skills cited by DOC-15
(`intent-conformance.md:13`, §2.4, §6.4) that **do not yet exist** — this spec
subsumes their normative role and names their authoring as a follow-up (Annex B, §10).

> **Scope note.** This is a **pure documentation standard** — a set of conventions
> for how any repository, in any language, declares machine-and-human-readable
> links from source artifacts (code, config, or Markdown) to the spec sections that
> govern them. The normative body (§§0–8) defines the standard itself: the
> reference grammar, the frontmatter schema, the per-language declaration idioms,
> and the spec-authoring conventions (numbering, header, anchors, lifecycle). **It
> binds no implementation** — it does not require any particular resolver, tool, or
> language runtime, and it is not scoped to trusty-tools' Rust codebase. Where this
> document illustrates the standard using trusty-tools as the reference adopter
> (the `docs/specs/` catalog, `trusty_common::intent_source`), those illustrations
> are marked **informative** and live in the annexes (Annex A, Annex B) and in §9,
> not in the normative standard. The PR carrying this doc opens **no** Rust changes.

---

## 0. Terminology

| Term | Meaning |
|---|---|
| **SLD (Spec-Linked Documentation)** | The convention by which a source artifact **declares** which spec section governs it, so a resolver can trace the artifact back to that section. SLD is a *linkage* convention, not a coverage or enforcement mechanism. |
| **Spec** | A behavior-contract document, carrying a document number and one or more `SPEC-{SUBSYSTEM}-{NN}~{rev}` IDs, each anchored with a `{#SPEC-…}` heading. Specs are the **targets** of SLD references. (Any repository may hold its specs wherever it chooses; trusty-tools' instance is `docs/specs/`, §9.) |
| **Source artifact** | Any tracked file that can *declare* a spec reference: code in any language, config, or a **Markdown** document. Sources are the **origins** of SLD references. |
| **Spec reference** | A single declared link from a source artifact to a spec section: a `(spec-ID, relative-path, anchor)` triple (§2). |
| **`# Spec References` block** | The delimited region in a non-Markdown source artifact that carries one or more spec references, expressed in that file type's comment/docstring idiom (§3). Markdown documents instead declare references in frontmatter (§2.5, §3.6). |
| **Anchor** | The `{#SPEC-{SUBSYSTEM}-{NN}~{rev}}` heading marker on a governed spec section; the link target of a spec reference. |
| **Resolver** | Any tool — in any language, in any repository — that reads declared SLD references (inline blocks or frontmatter) to trace a source artifact back to its governing spec section(s). A resolver is a **reader** of declared links only; this standard does not mandate a specific resolver implementation (see Annex A for one informative example). |
| **Declared-links-only** | SLD asserts a link **only** where a source artifact explicitly declares one. A resolver never *invents* linkage, never infers it from names/paths, and never treats absence as an error. |

**Canonical marker phrase (normatively fixed in §2.3):** every inline `# Spec
References` block is introduced by a line whose text, **after** stripping the
file's comment/docstring lead-in and any leading Markdown `#` heading markers, is,
**case-insensitively**, exactly `Spec References`. This single phrase is the block
marker in every non-Markdown language (§3); Markdown documents use frontmatter
instead (§2.5). Fenced code blocks are never scanned for markers (§2.3).

---

## 1. Purpose & scope

### 1.1 What this spec establishes

1. **SLD as a first-class, standalone standard** — the linkage convention is
   normative here, not incidental README prose, and is not bound to any one
   language, tool, or repository. The trusty-tools README defers to this document
   as canonical for its own use of SLD (§9).
2. **Language-agnostic linkage** — a comment-idiom-per-language table (§3) plus a
   **single canonical reference grammar** (§2), with two representations: an
   inline comment/docstring form (code, config) and a **YAML frontmatter form**
   (Markdown, §2.5), so the same triple is declared uniformly everywhere.
3. **Spec-document conventions** — DOC-N assignment, the header field block, the
   ID grammar, anchors, the status lifecycle, and the catalog-row requirement (§4).
4. **The authoring-skills contract** — the responsibilities the dangling
   `spec-authoring` / `spec-linked-docs` skills were meant to own (Annex B).

A concrete resolver implementation (e.g. trusty-common's `intent_source`) is
**informative only** (Annex A) — implementing it is out of scope for this
standard and is tracked as a follow-up (§10, F1).

### 1.2 Goals

- **G1 — One grammar, every language.** A source↔spec reference has the *same*
  `(spec-ID, path, anchor)` shape everywhere; only the surrounding idiom differs
  (an inline comment marker for code/config, frontmatter for Markdown). A
  conforming resolver needs one reference grammar plus a small per-idiom lookup —
  not a bespoke parser per language.
- **G2 — Rust unchanged.** The existing Rust rustdoc `# Spec References` form is
  the reference idiom for code and is preserved byte-for-byte (§3.1); no existing
  Rust link must be rewritten.
- **G3 — Structured and machine-parseable for documents.** Markdown documents
  declare their canonical references in **YAML frontmatter** (`spec_refs:`, §2.5)
  — a structured, directly machine-parseable form requiring no regex — with an
  optional visible section retained for human readability (§3.6).
- **G4 — Declared-links-only, reader-only.** SLD never invents linkage; a
  conforming resolver reads declared links and never enforces a traceability
  model (carried from DOC-15 §1.3).
- **G5 — Conventions in one place, portable.** DOC-N/ID/anchor/lifecycle rules are
  defined generically in this spec (§4) so any repository can adopt them; §9
  documents trusty-tools' own instantiation separately from the standard itself.

### 1.3 Non-goals (see §6)

Carried forward from DOC-15 §1.3 and made explicit here:

- **No four-status traceability enforcement.** SLD does **not** compute or gate on
  COVERED / UNCOVERED / ORPHANED / OUTDATED. A resolver is a *reader* of declared
  links, not a coverage enforcer.
- **No resolver implementation is normative.** Resolver behavior is illustrated
  informatively in Annex A; implementing any resolver (in Rust or otherwise) is a
  follow-up (§10, F1), not part of this standard.
- **No auto-insertion of references.** SLD never writes reference blocks or
  frontmatter into source; authors declare links deliberately (declared-links-only, G4).
- **No per-symbol coverage metrics, no CI traceability pass, no link linter** in
  this spec (each is a possible future follow-up, not normative here).
- **No renumbering of existing files in place.** The DOC-28 self-label collision
  (trusty-tools-specific, §9) is fixed by a follow-up (§10, F3), not by editing
  the colliding file here.

---

## 2. Canonical spec-reference grammar (one grammar, all languages) {#SPEC-SLD-02~draft}

**ID:** SPEC-SLD-02~draft
**Status:** Draft

A spec reference is a `(spec-ID, relative-path, anchor)` triple. It has two
normative representations: an **inline textual form** (§2.1–§2.4, used by code,
config, and optionally Markdown for human visibility) and a **frontmatter
structured form** (§2.5, canonical for Markdown documents).

### 2.1 The inline grammar (NORMATIVE)

```
SPEC-ID   := "SPEC-" SUBSYSTEM "-" NN "~" REV
SUBSYSTEM := /[A-Z0-9][A-Z0-9-]*/          ; e.g. SLD, CONFORMANCE, CFGDIR
NN        := /[0-9]{2,}/                    ; zero-padded, opaque per-spec ID counter
REV       := "draft" | /[a-z][a-z0-9]*/     ; ~draft, ~v1, ~v2, ~approved, … (see note below)
PATH      := /[A-Za-z0-9._\/-]+\.md/        ; canonical form is repo-root-relative, e.g. docs/specs/intent-conformance.md — see path-form note below
ANCHOR    := SPEC-ID                        ; equals the {#SPEC-…} heading anchor
TARGET    := PATH "#" ANCHOR
REF       := SPEC-ID  SEP  TARGET           ; SEP = markdown-link punctuation OR whitespace
```

- **`ANCHOR` MUST equal the `SPEC-ID`** it points at — the anchor is the section's
  `{#SPEC-{SUBSYSTEM}-{NN}~{rev}}` heading marker. This redundancy makes a reference
  self-checking: the leading ID and the trailing anchor must match.
- **`{NN}` is an opaque per-spec ID counter**, not a section-order index — a spec's
  IDs need not appear in the same order as their sections (e.g. this spec's
  `SPEC-SLD-01` anchors §4, not §1; §2's ID is `SPEC-SLD-02`). Do not infer document
  structure from ID order.
- **`{REV}` is an open, lowercase alphanumeric token**, not a closed `draft`/`v<N>`
  enumeration. `draft` and `v<N>` (`~v1`, `~v2`, …) are the standard's own baseline
  progression (§4.4), but adopting repositories MAY use additional named
  accepted-state tags — trusty-tools' DOC-36 uses `~approved` in place of `~v1` for
  six sections today. A resolver MUST NOT reject a well-formed `~<token>` revision
  it does not recognize.
- **`PATH` canonical form is repo-root-relative** (e.g. `docs/specs/foo.md`), not
  relative to the source file — so the reference is unambiguous regardless of where
  the source lives (matches the existing Rust convention). **This is the form a
  resolver parses as ground truth.** A *file-relative* URL (needed for a link to
  actually navigate on GitHub, which resolves relative links from the current
  file's directory) is permitted only in an **optional, informative** visible
  section (§3.6) — never in frontmatter `spec_refs` (§2.5) or in an inline
  `# Spec References` block, and never treated as canonical when it disagrees with
  a repo-root-relative declaration. This reconciles two constraints that otherwise
  conflict: machine resolution needs an unambiguous root-relative path, but GitHub
  rendering needs a file-relative one.

### 2.2 Two inline surface forms, one parse

The `REF` production admits two surface forms so Rust stays unchanged (G2) while
non-code files stay natural:

- **Markdown-link form (canonical for doc-comments):** the ID as link text, the
  target as link URL — the form Rust already uses:
  ``[`SPEC-SLD-02~draft`](docs/specs/spec-linked-documentation.md#SPEC-SLD-02~draft)``
- **Bare form (for plain comments / config):** ID, whitespace, target:
  `SPEC-SLD-02~draft docs/specs/spec-linked-documentation.md#SPEC-SLD-02~draft`

Both are parsed by **one** regex that captures the three fields and ignores
link/comment punctuation between them:

```
(SPEC-[A-Z0-9][A-Z0-9-]*-[0-9]{2,}~[a-z][a-z0-9]*)   # leading ID (REV is an open token: draft, v1, approved, …)
 [^\n]*?                                               # link/comment noise
 ([A-Za-z0-9._/-]+\.md)#                               # path (repo-root-relative when canonical, §2.1)
 (SPEC-[A-Z0-9][A-Z0-9-]*-[0-9]{2,}~[a-z][a-z0-9]*)   # anchor (== ID)
```

### 2.3 Block marker (NORMATIVE, inline form)

Inline references are grouped under a **`# Spec References` block** (§0). The
block marker is the line whose stripped text is, **case-insensitively**, exactly
`Spec References` (comment-lead and Markdown-`#` agnostic — matches the existing
`eq_ignore_ascii_case("Spec References")` behavior this standard's Rust idiom is
required to preserve, §3.1/G2). References belong to the nearest preceding marker
within the same comment/docstring region. A source artifact MAY declare multiple
blocks (e.g. one per function in Rust); each is resolved independently.

- **Fenced code blocks are excluded (NORMATIVE).** A marker line or reference
  occurring inside a fenced code block (delimited by ```` ``` ```` or `~~~`) is
  **not** a declaration and MUST be skipped when scanning. This applies both to
  source-code files (a docstring or comment might quote an example inside its own
  fence) and to Markdown's optional visible section (§3.6). This spec's own body
  contains many fenced `# Spec References` / `## Spec References` example lines —
  illustrations of the idiom, not real declarations — and a conforming scanner
  MUST NOT extract references from them.

### 2.4 Rationale (WHY, §2.1–§2.4)

A single `(ID, path, anchor)` triple with a self-checking anchor lets one resolver
serve every language: the hard part (find the ID, the path, the anchor) is
language-independent; the easy part (recognize the comment lead-in) is a small
per-idiom lookup (§3). Admitting both a Markdown-link and a bare inline form means
the established Rust rustdoc links need **no** rewrite (G2) while a shell script or
a YAML file can carry a reference without pretending to be Markdown.

### 2.5 The frontmatter form (NORMATIVE, canonical for Markdown)

Markdown documents (including specs themselves) declare their spec references
canonically in **YAML frontmatter** — a `---`-delimited block at the very top of
the file, before any other content — under a `spec_refs:` key. This is the
**structured equivalent** of the `(spec-ID, path, anchor)` triple in §2.1: the
same three fields, expressed as YAML rather than parsed out of prose text.

**Schema (NORMATIVE):**

```yaml
---
spec_refs:
  - id: <SPEC-ID>          # required; matches the SPEC-ID grammar, §2.1
    path: <relative-path>  # required; MUST be repo-root-relative (canonical form, §2.1) — never a file-relative GitHub-navigation URL
    anchor: <SPEC-ID>      # required; MUST equal `id` (self-check, mirrors §2.1's ANCHOR == SPEC-ID rule)
    note: <string>         # optional; free-text annotation, not parsed by a resolver
---
```

- Each list entry under `spec_refs:` is a **structured map** with required keys
  `id`, `path`, `anchor` (and an optional `note`). This is the **canonical** entry
  form: it requires no regex, only a YAML parser, and every field is named and
  unambiguous.
- **Shorthand (accepted, not canonical):** an entry MAY instead be a bare scalar
  string using exactly the §2.2 bare-form grammar, parsed with the **same** regex
  used for inline references:
  ```yaml
  ---
  spec_refs:
    - "SPEC-SLD-02~draft docs/specs/spec-linked-documentation.md#SPEC-SLD-02~draft"
  ---
  ```
  A resolver MUST accept both forms in the same list; a mapping entry is read
  directly (no regex), and a scalar entry is parsed via §2.2's regex. Either form
  yields the identical `(id, path, anchor)` triple.
- **Placement and scope.** The `spec_refs:` frontmatter block is the **canonical,
  authoritative** declaration of a Markdown document's outbound spec references.
  It governs the **whole document** (there is no per-section frontmatter). A
  document MAY additionally carry an informative visible section (§3.6) for human
  readability; that section is **not** authoritative where it disagrees with
  frontmatter.
- **Path form (NORMATIVE — resolves the root-relative-vs-GitHub tension).**
  Frontmatter `path` fields carry the **canonical, repo-root-relative** path
  (§2.1) — this is the only form a resolver treats as ground truth. Repo-root-relative
  paths do not click through as working GitHub hyperlinks (GitHub resolves
  relative link URLs against the current file's directory, not the repo root), so
  an optional visible section (§3.6) MAY instead use a **file-relative** link URL
  purely so the rendered link navigates on github.com. A file-relative URL in a
  visible section is informative only and is never parsed as the canonical path;
  where frontmatter and a visible section disagree, frontmatter wins.
- **Relationship to a spec's metadata header.** A spec document's existing visible
  bold-field header block (Status/Subsystem/Owner/… , §4.2) is **unaffected** by
  this convention and is **not** duplicated into frontmatter. Frontmatter, when
  present, carries only `spec_refs:` (and any future reference-related keys); it
  is not a replacement for the bold-field metadata block, and it precedes it at
  the top of the file.

### 2.6 Rationale (WHY, §2.5 — frontmatter as canonical for Markdown)

YAML frontmatter is directly machine-parseable (a YAML parser, not a regex, reads
it) and is a widely recognized convention for document metadata (static-site
generators, documentation tooling). Making it canonical for `spec_refs` keeps the
document body free of a mandatory reference section while still allowing one for
human readers (§3.6). Structured maps are the canonical entry form because they
are self-describing and need no parsing beyond YAML itself; the bare-string
shorthand is retained as an accepted alternative so a single reference regex
(§2.2) can, where convenient, serve both the inline and the frontmatter form.
**Honesty note:** as of this revision, the existing specs under `docs/specs/`
(including prior drafts of this one) do **not** yet carry `spec_refs` frontmatter;
retrofitting them is a follow-up (§10, F5/F6), not a precondition for adopting the
standard going forward. This spec itself now carries frontmatter (top of file) to
comply with its own convention (§9).

---

## 3. Language-agnostic linkage idioms (per file type) {#SPEC-SLD-03~draft}

**ID:** SPEC-SLD-03~draft
**Status:** Draft

Each file type declares spec references in its **native** idiom: an inline
`# Spec References` comment/docstring block (§2.1–§2.4) for code and config, or
YAML frontmatter (§2.5) for Markdown. Examples use `SPEC-SLD-03~draft`.

| File type | Extensions | Idiom | Declaration form |
|---|---|---|---|
| **Rust** | `.rs` | rustdoc (**unchanged**) | inline: module `//! # Spec References`; item `/// # Spec References` |
| **Python** | `.py` | PEP-257 docstring | inline: a `Spec References` section inside the module or symbol `"""…"""` docstring |
| **TypeScript / JavaScript** | `.ts`,`.tsx`,`.js`,`.mjs`,`.cjs` | JSDoc | inline: a `/** … */` block above the module or symbol |
| **Shell** | `.sh`,`.bash` | leading comment block | inline: a `#`-comment block near the top of the file |
| **TOML / YAML** | `.toml`,`.yaml`,`.yml` | comment block | inline: a `#`-comment block (top of file or above the governed key) |
| **Markdown** | `.md` | **YAML frontmatter** | `spec_refs:` frontmatter (canonical, §2.5); optional visible section for readers (§3.6) |

### 3.1 Rust (`.rs`) — reference idiom, UNCHANGED

```rust
//! # Spec References
//!
//! - [`SPEC-SLD-03~draft`](docs/specs/spec-linked-documentation.md#SPEC-SLD-03~draft)
```

Function-level blocks use `///` identically. This is exactly today's convention
(README §SLD, DOC-15 §2.4); nothing changes for Rust.

### 3.2 Python (`.py`)

```python
def deploy() -> None:
    """Deploy the service.

    # Spec References
    - SPEC-SLD-03~draft docs/specs/spec-linked-documentation.md#SPEC-SLD-03~draft
    """
```

The block lives inside the module docstring (module-level governance) or a
function/class docstring (symbol-level). Bare form is used since docstrings are not
Markdown-rendered by default.

### 3.3 TypeScript / JavaScript (`.ts`, `.js`, …)

```ts
/**
 * # Spec References
 * - SPEC-SLD-03~draft docs/specs/spec-linked-documentation.md#SPEC-SLD-03~draft
 */
export function deploy(): void {}
```

A resolver strips the leading `*` (and `/**`, `*/`) before applying the marker
and reference grammar.

### 3.4 Shell (`.sh`, `.bash`)

```sh
#!/usr/bin/env bash
# Spec References
# - SPEC-SLD-03~draft docs/specs/spec-linked-documentation.md#SPEC-SLD-03~draft
```

The comment lead-in is `#`; after stripping it the marker line reads
`Spec References`. The block SHOULD appear near the top, after any shebang.

### 3.5 TOML / YAML (`.toml`, `.yaml`, `.yml`)

```toml
# Spec References
# - SPEC-SLD-03~draft docs/specs/spec-linked-documentation.md#SPEC-SLD-03~draft

[server]
port = 8080
```

Same `#`-comment idiom as shell. A block at the top of the file governs the whole
file; a block immediately above a table/key governs that key (nearest-preceding-
marker scoping, §2.3). Note: this `#`-comment idiom is distinct from Markdown's
frontmatter convention (§3.6) — TOML/YAML files are not treated as Markdown.

### 3.6 Markdown (`.md`) — canonical form: `spec_refs:` frontmatter

A Markdown document declares its governing spec(s) canonically via **YAML
frontmatter** (§2.5):

```markdown
---
spec_refs:
  - id: SPEC-SLD-03~draft
    path: docs/specs/spec-linked-documentation.md
    anchor: SPEC-SLD-03~draft
---

# My Document
…
```

**Decision (NORMATIVE): `spec_refs:` YAML frontmatter is the canonical Markdown
declaration form. A visible `## Spec References` section is optional and
informative — permitted for human-reader visibility, but not authoritative.**

- **Why frontmatter is canonical.** (1) It is directly machine-parseable — a YAML
  parser reads it with no bespoke regex, unlike a prose section. (2) It is a single,
  predictable location (top of file) rather than a section that could appear
  anywhere in the body. (3) It keeps the document body free to organize its own
  content without a mandatory reference section competing for space.
- **Optional visible echo.** A document MAY additionally carry a visible section
  for readers who do not inspect raw source:
  ```markdown
  ## Spec References

  - [`SPEC-SLD-03~draft`](spec-linked-documentation.md#SPEC-SLD-03~draft)
  ```
  This section, when present, SHOULD be kept consistent with the frontmatter but
  is **informative only** — a resolver reads `spec_refs:` frontmatter as the
  source of truth, never this section. Heading level SHOULD be `##` (H2), not `#`,
  so it never competes with the document title. **Note the example's link URL is
  deliberately file-relative** (`spec-linked-documentation.md`, not
  `docs/specs/spec-linked-documentation.md`) so it actually navigates on GitHub
  (§2.1's path-form note) — the frontmatter entry for the same reference carries
  the canonical repo-root-relative path instead.
- **A document with no frontmatter and no visible section** declares no spec
  references (declared-links-only, G4) — this is a valid, unremarkable state, not
  an error.

### 3.7 Rationale (WHY)

Meeting each language in its **native** idiom (rustdoc, PEP-257, JSDoc, `#`
comments, or YAML frontmatter for Markdown) means SLD adds no foreign syntax an
author must learn per language — the only things to remember are the phrase `Spec
References` for inline idioms (§2.3) and the `spec_refs:` key for Markdown (§2.5).
Choosing **frontmatter** as canonical for Markdown, rather than a body section,
keeps machine parsing simple and predictable while an optional visible section
still gives human readers on-page context.

---

## 4. Spec-document conventions (the standard's authoring rules)

> These conventions define how any adopting repository numbers, headers, anchors,
> and catalogs its specs. They are generic to the standard; trusty-tools'
> concrete instantiation of them is documented separately in §9.

### 4.1 DOC-N assignment (NORMATIVE) {#SPEC-SLD-01~draft}

**ID:** SPEC-SLD-01~draft
**Status:** Draft

- Every spec carries a unique document number (`DOC-N` in trusty-tools' instance)
  within its repository's spec namespace. **Numbers are assigned scan-before-claim:**
  before claiming a number, scan the **entire** spec-bearing portion of the
  repository — the catalog **and** any self-labeled spec headers outside the
  catalog — for the highest claimed number, and claim the next genuinely free
  integer.
  - A catalog's "next free number" note is a **hint, not authority**; it drifts
    stale. The scan is authoritative.
  - Where a repository maintains more than one independent numbering namespace
    (e.g. trusty-tools' `docs/specs/` `DOC-N` series is separate from
    `docs/trusty-installer/research/**`'s own doc-charter numbering), do not
    conflate namespaces when scanning.
- **Collision handling.** A document-number collision (two files self-labeling the
  same number) is resolved by assigning the **uncataloged** file the next free
  number and adding a catalog row — never by silently reusing a number. The
  motivating case (trusty-tools): `mpm-cutover-resume-native-optimization.md`
  self-labels **DOC-28**, which collides with the canonical DOC-28
  (`trusty-mpm-self-awareness.md`); the fix is a follow-up (§10, F3), and until
  then the collision is flagged in the catalog.

### 4.2 Header field block (NORMATIVE)

A spec opens with a title heading (`# DOC-N — <Title>` in trusty-tools' instance)
followed by a **visible bold-field block**:

| Field | Required | Meaning |
|---|---|---|
| **Status** | yes | `Draft` \| `Accepted` \| `Superseded` (the document's overall lifecycle state; individual sections may carry their own §4.4). |
| **Subsystem** | yes | The area/component(s) the spec governs. |
| **Owner** | yes | The owning team/role. |
| **Last-updated** | yes | ISO date of the last substantive edit. |
| **Spec ID** | yes | The `SPEC-{SUBSYSTEM}-{NN}~{rev}` ID(s) this doc defines (a single ID or an `… … …` range). |
| **Epic** | optional | The umbrella issue/epic. |
| **Builds on** | optional | Prior specs / material this one depends on. |
| **Cross-ref** | optional | Related specs, issues, or implementation seams. |

This bold-field block carries the spec's **metadata**. It is unaffected by, and
never duplicates, the `spec_refs:` frontmatter (§2.5): a spec MAY carry both — YAML
frontmatter for its outbound references, and the bold-field block, immediately
below the frontmatter, for its own metadata. Where a spec has no outbound
references to declare, frontmatter is simply omitted.

### 4.3 ID grammar & anchors (NORMATIVE)

- Spec IDs follow `SPEC-{SUBSYSTEM}-{NN}~{rev}` (§2.1). `{SUBSYSTEM}` is a short
  uppercase token unique to the spec's area (e.g. `SLD`, `CONFORMANCE`, `CFGDIR`);
  `{NN}` is a zero-padded, **opaque** per-spec ID counter — it enumerates a spec's
  IDs, not its section/heading order (§2.1); `{rev}` is `draft` initially.
- Every **governed** section anchors its ID with a `{#SPEC-{SUBSYSTEM}-{NN}~{rev}}`
  heading marker, so SLD references (§2) can target it. A section without an anchor
  cannot be an SLD target.
- **GitHub-rendering caveat (informative, honest disclosure).** GitHub-Flavored
  Markdown does **not** support explicit `{#custom-id}` heading attributes — GitHub
  renders the literal `{#SPEC-…}` text and auto-generates its own anchor by
  slugifying the visible heading text instead. A `path.md#SPEC-…` cross-link
  therefore **will not navigate correctly on github.com**; the `{#…}` convention
  targets non-GitHub tooling that does honor explicit heading IDs (e.g. mkdocs,
  pandoc, Sphinx-style renderers) and, more importantly, a **resolver that parses
  the `{#…}` marker directly out of the source text** rather than relying on
  rendered HTML anchors (§2.1's `ANCHOR := SPEC-ID` rule is about that textual
  marker, not about GitHub's rendered anchor). On github.com, treat `{#SPEC-…}`
  cross-links as **best-effort** — they name the right file and the right ID for a
  human to locate by scanning, even when the browser doesn't jump there.

### 4.4 Status lifecycle & rev-suffix semantics (NORMATIVE)

- A section's status is `Draft → Accepted → Superseded`.
- The `~draft` suffix marks an unfrozen section. On acceptance the suffix becomes a
  version tag: `~draft → ~v1`, and subsequent accepted revisions increment
  (`~v1 → ~v2`). Draft sections are linkable via SLD **while still `~draft`** — so
  traceability is established from the first implementing change.
- **Named accepted-state tags are permitted alongside `~v<N>`.** The `{REV}` token
  is open (§2.1); an adopting repository MAY use a named tag such as `~approved`
  in place of `~v1`/`~v2`/… to mark acceptance — trusty-tools' DOC-36 does this
  today (`SPEC-TMMGR-01~approved` … `-06~approved`). Both progressions mean the
  same thing (the section left `~draft`); this standard does not mandate which
  accepted-state spelling a repository uses, only that `~draft` is the universal
  starting state.
- **When a section's `{rev}` changes, its anchor changes**, so inbound SLD
  references naming the old rev become revision-drift signals. A conforming
  resolver MAY still resolve the reference to the current section and flag drift;
  **enforcing** drift (OUTDATED) is a non-goal (§1.3).

### 4.5 Catalog-row requirement (NORMATIVE)

Every spec MUST have a row in its repository's spec catalog (in trusty-tools:
the `docs/specs/README.md` "Spec catalog" table — `DOC-N | Spec ID(s) | Title
(link) | Subsystem`). A spec file without a catalog row is **uncataloged** and is
a defect to be fixed (add the row), not a valid state.

---

## 5. Acceptance criteria (testable, standard-level)

- **AC-1** A new spec assigned by scan-before-claim (§4.1) takes the next integer
  above the highest document number found anywhere in the repository's spec
  namespace (catalog + self-labeled), and adds a catalog row (§4.5).
- **AC-2** A spec's header is a visible bold-field block (§4.2) with all required
  fields; `spec_refs:` frontmatter, if present, precedes it and carries only
  reference data (no duplicated metadata).
- **AC-3** Every governed section carries a `{#SPEC-…}` anchor equal to its ID (§4.3).
- **AC-4** The inline reference regex (§2.2) extracts the same `(ID, path, anchor)`
  triple from the Markdown-link form and the bare form.
- **AC-5** An inline `# Spec References` block is recognized in each non-Markdown
  file type of §3 (Rust, Python, TS/JS, shell, TOML/YAML) via the stripped-phrase
  marker (§2.3).
- **AC-6** A Markdown document's `spec_refs:` frontmatter resolves to the same
  `(id, path, anchor)` triples whether entries are structured maps or bare-string
  shorthand (§2.5); an optional visible section, if present, is informative only;
  a document with neither frontmatter nor a visible section yields no reference.
- **AC-7** An existing Rust `//! # Spec References` block resolves **unchanged**
  (no regression — G2).

---

## 6. Non-goals

- Four-status traceability enforcement (COVERED/UNCOVERED/ORPHANED/OUTDATED) — a
  resolver reads links; it does not gate on coverage (carried from DOC-15 §1.3).
- Implementing any resolver, in Rust or any other language — resolver behavior is
  illustrated informatively (Annex A); implementation is a follow-up (F1).
- Auto-inserting or auto-rewriting reference blocks or frontmatter into any source.
- A CI SLD link-linter / dead-anchor check (a possible future follow-up, not
  normative here).
- Per-symbol spec-coverage metrics or a traceability matrix.
- Renumbering the DOC-28-colliding file in place (fixed by follow-up F3;
  trusty-tools-specific, §9).
- Governing *which* code or documents must be spec-linked — SLD defines *how* to
  link when an author chooses to; it does not mandate coverage.
- Mandating any specific repository layout, spec-catalog location, or programming
  language — the standard is portable; §9 documents one adopter's (trusty-tools')
  instantiation.

---

## 7. Worked example (non-normative)

A Python module declaring an inline reference, and a Markdown document declaring
the equivalent reference via frontmatter — both resolve to the same triple:

```python
"""Deployment helpers.

# Spec References
- SPEC-SLD-03~draft docs/specs/spec-linked-documentation.md#SPEC-SLD-03~draft
"""
```

```markdown
---
spec_refs:
  - id: SPEC-SLD-03~draft
    path: docs/specs/spec-linked-documentation.md
    anchor: SPEC-SLD-03~draft
---

# Deployment Runbook
…
```

Both declare the identical triple `(SPEC-SLD-03~draft,
docs/specs/spec-linked-documentation.md, SPEC-SLD-03~draft)`, resolving to §3 of
this spec — one via the inline grammar (§2.1–§2.4), the other via the frontmatter
schema (§2.5). That equivalence is the whole point of the one-grammar,
two-representation design (§2.4, §2.6).

---

## 8. Change log

- **2026-07-16** — Initial draft (DOC-38, `SPEC-SLD-01~draft` … `SPEC-SLD-03~draft`).
  Promoted SLD from README prose to a first-class, language-agnostic spec per Bob's
  directive ("SLD converted to a first class spec that we use for ANY language or
  even markdown").
- **2026-07-16 (revision)** — Two binding design corrections from Bob: (1)
  **frontmatter is now the canonical Markdown declaration form** (`spec_refs:`
  YAML frontmatter, §2.5, §3.6), reversing the prior "visible section is
  canonical" decision; the visible `## Spec References` section is now optional/
  informative. (2) **Reframed as a pure, implementation-neutral standard** usable
  in any repository or language: the `trusty_common::intent_source` resolver
  material was demoted from core normative content to informative Annex A;
  section 5's prior "Resolver contract" is now non-normative and follow-up F1
  covers its implementation. DOC-38 now self-complies with its own frontmatter
  convention (top of this file). Added Annex A (informative resolver notes) and
  Annex B (informative authoring-skill notes); added follow-up F6 (frontmatter
  retrofit of existing specs).

---

## Annex A (Informative) — A reference resolver: trusty-common `intent_source`

> **This annex is informative, not normative.** It illustrates how one concrete
> resolver — trusty-common's `intent_source` (DOC-15 §6, `SPEC-CONFORMANCE-03~draft`)
> — could read SLD references per §2–§3 of this standard. Nothing in this annex is
> binding on adopters of the standard; the standard is defined entirely by §§0–8
> above. Implementing this (or any) resolver is tracked as follow-up **F1**.

### A.1 Behavior sketch

- **Input:** a changed file (path + contents) of any file type registered in a
  per-extension table (A.2).
- **Output:** zero or more `SpecRef { spec_id, path, anchor }` triples resolved to
  the governing `{#SPEC-…}` section(s). A file with no declared reference
  (no inline block, no `spec_refs:` frontmatter) yields **zero** refs.
- **Algorithm sketch (per file):**
  1. Look up the file's extension. Unknown extension → no refs (never guess).
  2. For non-Markdown types: strip the comment/docstring lead-in, find `# Spec
     References` block markers (§2.3), apply the §2.2 regex within each block.
  3. For Markdown: parse `spec_refs:` frontmatter (§2.5) directly as structured
     data (or via the §2.2 regex for bare-string shorthand entries); optionally
     cross-check against a visible section if present, informationally only.
  4. Resolve each anchor to the `{#SPEC-…}` heading in the referenced document. An
     anchor that does not equal its leading/declared `id`, or that resolves to no
     section, is dropped with a logged warning (declared-links-only; never
     fabricate a target).
- **Declared-links-only, reader-only, fail-open:** as specified generically in
  §1.2 (G4) and §1.3 — a sketch resolver never invents linkage, never writes
  references, and treats any parse/read failure as zero refs for that file (not
  a spurious ref).

### A.2 Illustrative per-extension table

| Extension(s) | Line-comment | Block / docstring delimiters |
|---|---|---|
| `.rs` | `//`, `///`, `//!` | — |
| `.py` | `#` | `"""…"""`, `'''…'''` |
| `.ts`,`.tsx`,`.js`,`.mjs`,`.cjs` | `//` | `/** … */`, `/* … */` |
| `.sh`,`.bash` | `#` | — |
| `.toml`,`.yaml`,`.yml` | `#` | — |
| `.md` | — (frontmatter, §2.5) | `---…---` frontmatter block |

This table would be the resolver's only language-specific surface; everything
downstream (marker, reference grammar, frontmatter schema, anchor resolution) is
shared and defined generically above.

### A.3 Illustrative conformance checks (non-normative; for F1)

- A file whose extension is not registered yields zero refs.
- A reference whose anchor does not equal its declared id, or whose anchor
  resolves to no section, is dropped with a warning.
- A file with no declared reference yields zero refs and is not treated as an error.
- The same `(id, path, anchor)` triple resolves identically whether declared
  inline (§2.1–§2.4) or via frontmatter (§2.5).

---

## Annex B (Informative) — Authoring-skill guidance (trusty-tools-specific)

> **This annex is informative and trusty-tools-specific**, not part of the
> portable standard. It records the responsibilities the dangling `spec-authoring`
> / `spec-linked-docs` skills (cited by DOC-15, `intent-conformance.md:13`, §2.4,
> §6.4, but **not present** in `.claude/skills/`, `crates/trusty-mpm/src/assets/`,
> or `~/.trusty-mpm/`) were meant to own, so that authoring them (follow-up F2) has
> a clear brief.

- **`spec-authoring` — responsibilities:** how to create a spec — scan-before-claim
  numbering (§4.1), the header field block (§4.2), the ID grammar and anchors
  (§4.3), the status lifecycle (§4.4), and the catalog-row requirement (§4.5). An
  author following §4 needs no separate skill to write a conformant spec; the F2
  skill is a checklist/entry-point that defers to §4.
- **`spec-linked-docs` (SLD) — responsibilities:** how to *link* a source artifact
  to a spec — the canonical reference grammar and frontmatter schema (§2), and the
  per-language declaration idioms (§3). The F2 skill is a per-language cheat-sheet
  that defers to §2/§3.

**Contract for the follow-up skills (F2):** each skill's SKILL.md MUST cite this
spec (`DOC-38`) as its normative source and MUST NOT restate normative rules — it
links to the section, so there is one source of truth and the skills cannot drift
from the spec. DOC-15's dangling citations SHOULD be updated to point at DOC-38
(and, once they exist, the skills) — tracked as F4.

---

## 9. Applying this standard in trusty-tools (this PR; trusty-tools-specific, not part of the generic standard)

- **Add the catalog row** for DOC-38 in `docs/specs/README.md`.
- **Rewrite the §"Spec-Linked Documentation (SLD)" section** to a short summary
  that names DOC-38 as canonical and links to it, instead of restating the
  Rust-only rustdoc convention as if it were the whole of SLD, and to reflect that
  frontmatter is now canonical for Markdown.
- **Fix the stale "next free DOC-N" note** in the DOC-28-collision catalog note:
  after the scan, DOC-34 is assigned (`managed-session-config-dir.md`), DOC-35/36
  are cataloged, DOC-37 is self-labeled (`trusty-search-managed-repo-awareness.md`,
  uncataloged), and **DOC-38 is claimed by this spec** — so the next free number for
  the DOC-28 renumber follow-up is **DOC-39**.
- **Self-compliance:** this document itself now carries `spec_refs:` frontmatter
  (top of file) pointing at DOC-15 §6 (`SPEC-CONFORMANCE-03~draft`), the section
  its (informative) Annex A material extends.
- **Honesty note:** every *other* existing spec under `docs/specs/` still lacks
  `spec_refs` frontmatter as of this PR. Retrofitting them is **not** required for
  this standard to take effect going forward; it is tracked as follow-up **F6**.

---

## 10. Follow-ups / child issues (for the PM to file)

Umbrella: **epic: spec-linked documentation, first-class & language-agnostic**
(issue to be linked by the PM).

| ID | Title | Scope | Deps | Effort |
|----|-------|-------|------|--------|
| **F1** | Build a reference SLD resolver (e.g. extend trusty-common `intent_source`) | Implement Annex A: per-extension lookup, inline block parsing, `spec_refs:` frontmatter parsing, anchor resolution, declared-links-only/fail-open. AC-4..AC-7, Annex A.3. | none (standard is self-contained) | **M** |
| **F2** | Author the `spec-authoring` + `spec-linked-docs` skills | Two SKILL.md files that cite DOC-38 as normative source and defer to §4 (authoring) / §2–§3 (linkage); no restated rules (Annex B). | DOC-38 (this spec) | **S** |
| **F3** | Fix the DOC-28 self-label collision (trusty-tools) | Assign `mpm-cutover-resume-native-optimization.md` the next free number (**DOC-39** per §9) and add a catalog row; leave canonical DOC-28 untouched (§4.1). | scan (§4.1) | **S** |
| **F4** | Update DOC-15's dangling skill citations | Repoint `intent-conformance.md:13`, §2.4, §6.4 from the non-existent `spec-linked-docs`/`spec-authoring` to DOC-38 (and the F2 skills once they exist). | F2 | **S** |
| **F5** | Retrofit existing non-Rust source with inline SLD blocks (opt-in, incremental) | Where a non-Rust, non-Markdown file is clearly governed by a spec, add a `# Spec References` block in its native idiom (§3). Declared-links-only — no bulk auto-insertion. | F1 | **M** (incremental) |
| **F6** | Retrofit existing `docs/specs/*.md` (and other Markdown) with `spec_refs` frontmatter | Add `spec_refs:` frontmatter to existing specs/docs that declare inbound linkage today only in prose ("Builds on"/"Cross-ref") or not at all. Declared-links-only — no bulk auto-insertion; each entry added deliberately where a real reference exists. | F1 | **M** (incremental) |

**Suggested order:** F1 ∥ F2 → F3 → F4 → (F5 ∥ F6, continuous thereafter).
