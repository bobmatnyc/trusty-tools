# DOC-38 — Spec-Linked Documentation (SLD): Language-Agnostic Source↔Spec Linkage

**Status:** Draft
**Subsystem:** cross-crate — `docs/specs` conventions + trusty-common `intent_source` resolver (all languages)
**Owner:** Engineering (trusty-tools platform)
**Last-updated:** 2026-07-16
**Spec ID:** `SPEC-SLD-01~draft` … `SPEC-SLD-05~draft` (DOC-38)
**Epic:** epic: spec-linked documentation, first-class & language-agnostic (umbrella issue to be linked by the PM)
**Builds on:**
- The SLD prose and spec-catalog conventions in [`docs/specs/README.md`](./README.md)
  (DOC-N numbering, `SPEC-{SUBSYSTEM}-{NN}~{rev}` grammar, `{#SPEC-…}` anchors,
  Draft→Accepted→Superseded lifecycle, the Rust `# Spec References` rustdoc form).
- DOC-15 [Intent / Method Conformance](./intent-conformance.md) — the intent-source
  resolver (ISR, `trusty_common::intent_source`) that *reads* declared SLD links to
  resolve a changed file back to its governing spec section (§2.4, §6.4).
**Cross-ref:** the `spec-authoring` / `spec-linked-docs` skills cited by DOC-15
(`intent-conformance.md:13`, §2.4, §6.4) that **do not yet exist** — this spec
subsumes their normative role and names their authoring as a follow-up (§9, §11).

> **Scope note.** This is a **conventions + behavior-contract** spec. It promotes
> Spec-Linked Documentation (SLD) from scattered README prose to a first-class,
> **language-agnostic** spec: it fixes the spec-document conventions, defines a
> single canonical source↔spec reference grammar that works in **any** language
> (and in Markdown), and states the resolver contract that reads those references.
> It **implements nothing**: the PR carrying this doc opens **no** Rust changes.
> The resolver extension, the authoring skills, the DOC-28 collision fix, and the
> retrofit of existing non-Rust files are decomposed into follow-up issues (§11).

---

## 0. Terminology

| Term | Meaning |
|---|---|
| **SLD (Spec-Linked Documentation)** | The convention by which a source artifact **declares** which spec section governs it, so tooling can resolve the artifact back to that section. SLD is a *linkage* convention, not a coverage or enforcement mechanism. |
| **Spec** | A behavior-contract document under `docs/specs/`, carrying a `DOC-N` number and one or more `SPEC-{SUBSYSTEM}-{NN}~{rev}` IDs, each anchored with a `{#SPEC-…}` heading. Specs are the **targets** of SLD references. |
| **Source artifact** | Any tracked file that can *declare* a spec reference: Rust/Python/TS/JS/shell code, TOML/YAML config, or a **Markdown** document. Sources are the **origins** of SLD references. |
| **Spec reference** | A single declared link from a source artifact to a spec section: a `(spec-ID, relative-path, anchor)` triple (§2). |
| **`# Spec References` block** | The delimited region in a source artifact that carries one or more spec references, expressed in that file type's comment/docstring idiom (§3). |
| **Anchor** | The `{#SPEC-{SUBSYSTEM}-{NN}~{rev}}` heading marker on a governed spec section; the link target of a spec reference. |
| **Intent-source resolver (ISR)** | `trusty_common::intent_source` (DOC-15 §6): the **reader** that parses `# Spec References` blocks out of changed files to find the governing spec section. It reads declared links only. |
| **Declared-links-only** | SLD asserts a link **only** where a source artifact explicitly declares one. Tooling never *invents* linkage, never infers it from names/paths, and never treats absence as an error. |

**Canonical marker phrase (normatively fixed in §2.3):** every `# Spec References`
block is introduced by a line whose text, **after** stripping the file's
comment/docstring lead-in and any leading Markdown `#` heading markers, is exactly
`Spec References`. This single phrase is the block marker in every language (§3), so
one resolver grammar recognizes them all.

---

## 1. Purpose & scope

### 1.1 What this spec establishes

1. **SLD as a first-class spec** — the linkage convention is normative here, not
   incidental README prose. The README defers to this document as canonical (§10).
2. **Language-agnostic linkage** — a comment-idiom-per-language table (§3) plus a
   **single canonical reference grammar** (§2) that a resolver parses uniformly
   across Rust, Python, TypeScript/JavaScript, shell, TOML/YAML, and **Markdown**.
3. **Spec-document conventions** — DOC-N assignment, the header field block, the
   ID grammar, anchors, the status lifecycle, and the catalog-row requirement,
   promoted from README prose to normative text (§4).
4. **A resolver contract** — how `trusty_common::intent_source` extends beyond
   `.rs` to any registered file type (§5), stated as behavior, code as follow-up.
5. **The authoring-skills contract** — the responsibilities the dangling
   `spec-authoring` / `spec-linked-docs` skills were meant to own (§9).

### 1.2 Goals

- **G1 — One grammar, every language.** A source↔spec reference has the *same*
  `(spec-ID, path, anchor)` shape everywhere; only the surrounding comment idiom
  differs. A resolver needs one reference grammar plus a per-extension comment
  table — not a bespoke parser per language.
- **G2 — Rust unchanged.** The existing Rust rustdoc `# Spec References` form is
  the reference implementation and is preserved byte-for-byte (§3.1); no existing
  Rust link must be rewritten.
- **G3 — Visible over hidden.** Markdown documents declare references in a
  **visible** `## Spec References` section, not hidden frontmatter or comments —
  consistent with specs' own no-YAML-frontmatter house style (§3.6, §4.2).
- **G4 — Declared-links-only, reader-only.** SLD never invents linkage; the ISR
  reads declared links and never enforces a traceability model (carried from
  DOC-15 §1.3).
- **G5 — Conventions in one place.** DOC-N/ID/anchor/lifecycle rules live in this
  spec; the README holds a summary and the catalog, not the normative rules.

### 1.3 Non-goals (see §8)

Carried forward from DOC-15 §1.3 and made explicit here:

- **No four-status traceability enforcement.** SLD/ISR does **not** compute or gate
  on COVERED / UNCOVERED / ORPHANED / OUTDATED. The ISR is a *reader* of declared
  links, not a coverage enforcer.
- **No resolver implementation in this PR.** §5 is a behavior contract; the code
  change is a follow-up (§11, F1).
- **No auto-insertion of references.** SLD never writes `# Spec References` blocks
  into source; authors declare links deliberately (declared-links-only, G4).
- **No per-symbol coverage metrics, no CI traceability pass, no link linter** in
  this spec (each is a possible future follow-up, not normative here).
- **No renumbering of existing files in place.** The DOC-28 self-label collision is
  fixed by a follow-up (§11, F3), not by editing the colliding file here.

---

## 2. Canonical spec-reference grammar (one grammar, all languages) {#SPEC-SLD-02~draft}

**ID:** SPEC-SLD-02~draft
**Status:** Draft

A spec reference is a `(spec-ID, relative-path, anchor)` triple. Its textual form
is identical across languages; only the enclosing comment idiom varies (§3).

### 2.1 The grammar (NORMATIVE)

```
SPEC-ID   := "SPEC-" SUBSYSTEM "-" NN "~" REV
SUBSYSTEM := /[A-Z0-9][A-Z0-9-]*/          ; e.g. SLD, CONFORMANCE, CFGDIR
NN        := /[0-9]{2,}/                    ; zero-padded section number
REV       := "draft" | "v" /[0-9]+/         ; ~draft, ~v1, ~v2, …
PATH      := /[A-Za-z0-9._\/-]+\.md/        ; relative to repo root, e.g. docs/specs/intent-conformance.md
ANCHOR    := SPEC-ID                        ; equals the {#SPEC-…} heading anchor
TARGET    := PATH "#" ANCHOR
REF       := SPEC-ID  SEP  TARGET           ; SEP = markdown-link punctuation OR whitespace
```

- **`ANCHOR` MUST equal the `SPEC-ID`** it points at — the anchor is the section's
  `{#SPEC-{SUBSYSTEM}-{NN}~{rev}}` heading marker. This redundancy makes a reference
  self-checking: the leading ID and the trailing anchor must match.
- **`PATH` MUST be repo-root-relative** (e.g. `docs/specs/foo.md`), not relative to
  the source file — so the reference is unambiguous regardless of where the source
  lives. (This matches the existing Rust convention, which writes `docs/specs/…`.)

### 2.2 Two surface forms, one parse

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
(SPEC-[A-Z0-9][A-Z0-9-]*-[0-9]{2,}~(?:draft|v[0-9]+))   # leading ID
 [^\n]*?                                                  # link/comment noise
 ([A-Za-z0-9._/-]+\.md)#                                  # relative path
 (SPEC-[A-Z0-9][A-Z0-9-]*-[0-9]{2,}~(?:draft|v[0-9]+))   # anchor (== ID)
```

### 2.3 Block marker (NORMATIVE)

References are grouped under a **`# Spec References` block** (§0). The block marker
is the line whose stripped text is exactly `Spec References` (comment-lead and
Markdown-`#` agnostic). References belong to the nearest preceding marker within
the same comment/docstring region. A source artifact MAY declare multiple blocks
(e.g. one per function in Rust); each is resolved independently.

### 2.4 Rationale (WHY)

A single `(ID, path, anchor)` triple with a self-checking anchor lets one resolver
serve every language: the hard part (find the ID, the path, the anchor) is
language-independent; the easy part (strip `//!` vs `#` vs `*`) is a small
per-extension table (§5). Admitting both a Markdown-link and a bare form means the
established Rust rustdoc links need **no** rewrite (G2) while a shell script or a
YAML file can carry a reference without pretending to be Markdown.

---

## 3. Language-agnostic linkage idioms (per file type) {#SPEC-SLD-03~draft}

**ID:** SPEC-SLD-03~draft
**Status:** Draft

Each file type declares a `# Spec References` block in its **native** comment or
docstring idiom. The block marker (§2.3) and reference grammar (§2.1) are identical
across all rows; only the lead-in differs. Examples use `SPEC-SLD-03~draft`.

| File type | Extensions | Idiom | Block placement |
|---|---|---|---|
| **Rust** | `.rs` | rustdoc (**unchanged**) | module `//! # Spec References`; item `/// # Spec References` |
| **Python** | `.py` | PEP-257 docstring | a `Spec References` section inside the module or symbol `"""…"""` docstring |
| **TypeScript / JavaScript** | `.ts`,`.tsx`,`.js`,`.mjs`,`.cjs` | JSDoc | a `/** … */` block above the module or symbol |
| **Shell** | `.sh`,`.bash` | leading comment block | a `#`-comment block near the top of the file |
| **TOML / YAML** | `.toml`,`.yaml`,`.yml` | comment block | a `#`-comment block (top of file or above the governed key) |
| **Markdown** | `.md` | **visible section** | a `## Spec References` H2 section (see §3.6) |

### 3.1 Rust (`.rs`) — reference implementation, UNCHANGED

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

The resolver strips the leading `*` (and `/**`, `*/`) before applying the marker
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
file; a block immediately above a table/key governs that key (resolver treats
nearest-preceding-marker scoping, §2.3).

### 3.6 Markdown (`.md`) — NORMATIVE form: a visible `## Spec References` section

A Markdown document declares its governing spec(s) in a **visible level-2 section**:

```markdown
## Spec References

- [`SPEC-SLD-03~draft`](spec-linked-documentation.md#SPEC-SLD-03~draft)
```

**Decision (NORMATIVE): the visible `## Spec References` section is the canonical
Markdown form; an HTML-comment block is a permitted fallback only.**

- **Why visible, not an HTML comment.** (1) Specs in this repo deliberately use a
  **visible bold-field header block, not YAML frontmatter** (§4.2) — hiding SLD
  links in `<!-- … -->` would contradict that house choice. (2) A visible section
  renders on GitHub and in docs sites, so a reader sees the governing spec without
  viewing source. (3) It shows up in PR diffs and review, so linkage is auditable
  where it is added or removed. (4) It is Markdown-native — a bulleted list of
  links needs no new tooling to render.
- **Heading level.** Use `##` (H2), **not** `#` (H1), so the block never competes
  with the document title. The resolver keys on the stripped phrase `Spec
  References` at **any** heading level (§2.3), so `##`/`###` both resolve.
- **Fallback.** Where a visible section is genuinely undesirable (e.g. a rendered
  end-user README where the section would be noise), an HTML-comment block is
  permitted:
  ```markdown
  <!-- Spec References
  - SPEC-SLD-03~draft docs/specs/spec-linked-documentation.md#SPEC-SLD-03~draft
  -->
  ```
  Authors SHOULD prefer the visible section; the fallback exists so the convention
  never forces visible noise into a polished document.

### 3.7 Rationale (WHY)

Meeting each language in its **native** idiom (rustdoc, PEP-257, JSDoc, `#`
comments) means SLD adds no foreign syntax an author must learn per language — the
only thing to remember is the phrase `Spec References` and the `(ID, path, anchor)`
triple. Choosing a **visible** Markdown section (over hidden frontmatter/comments)
keeps SLD consistent with the repo's existing no-frontmatter spec style and makes
doc↔spec linkage as reviewable as code↔spec linkage.

---

## 4. Spec-document conventions (promoted from README prose) {#SPEC-SLD-01~draft}

> These conventions were previously scattered across `docs/specs/README.md`. They
> are normative here; the README (§10) summarizes and defers to this section.

### 4.1 DOC-N assignment (NORMATIVE)

- Every spec carries a unique `DOC-N` document number in the `docs/specs/`
  namespace. **Numbers are assigned scan-before-claim:** before claiming a number,
  scan the **entire** `docs/` tree — the README catalog **and** any self-labeled
  spec headers outside the catalog — for the highest claimed `DOC-N`, and claim the
  next genuinely free integer.
  - The README's "next free DOC-N" note is a **hint, not authority**; it drifts
    stale (it has before). The scan is authoritative.
  - The `docs/specs/` DOC-N namespace is **separate** from the
    `docs/trusty-installer/research/**` doc-charter numbering (which has its own
    `00-naming-and-doc-charter.md`); do not conflate the two when scanning.
- **Collision handling.** A `DOC-N` collision (two files self-labeling the same
  number) is resolved by assigning the **uncataloged** file the next free number
  and adding a catalog row — never by silently reusing a number. The motivating
  case: `mpm-cutover-resume-native-optimization.md` self-labels **DOC-28**, which
  collides with the canonical DOC-28 (`trusty-mpm-self-awareness.md`); the fix is a
  follow-up (§11, F3), and until then the collision is flagged in the README.

### 4.2 Header field block (NORMATIVE — no YAML frontmatter)

A spec opens with an H1 title `# DOC-N — <Title>` followed by a **visible
bold-field block** (never YAML frontmatter):

| Field | Required | Meaning |
|---|---|---|
| **Status** | yes | `Draft` \| `Accepted` \| `Superseded` (the document's overall lifecycle state; individual sections may carry their own §4.4). |
| **Subsystem** | yes | The crate(s) / area the spec governs. |
| **Owner** | yes | The owning team/role. |
| **Last-updated** | yes | ISO date of the last substantive edit. |
| **Spec ID** | yes | The `SPEC-{SUBSYSTEM}-{NN}~{rev}` ID(s) this doc defines (a single ID or an `… … …` range). |
| **Epic** | optional | The umbrella issue/epic. |
| **Builds on** | optional | Prior specs / code this one depends on. |
| **Cross-ref** | optional | Related specs, issues, or code seams. |

### 4.3 ID grammar & anchors (NORMATIVE)

- Spec IDs follow `SPEC-{SUBSYSTEM}-{NN}~{rev}` (§2.1). `{SUBSYSTEM}` is a short
  uppercase token unique to the spec's area (e.g. `SLD`, `CONFORMANCE`, `CFGDIR`);
  `{NN}` is a zero-padded per-spec section counter; `{rev}` is `draft` initially.
- Every **governed** section anchors its ID with a `{#SPEC-{SUBSYSTEM}-{NN}~{rev}}`
  heading marker, so SLD references (§2) can target it. A section without an anchor
  cannot be an SLD target.

### 4.4 Status lifecycle & rev-suffix semantics (NORMATIVE)

- A section's status is `Draft → Accepted → Superseded`.
- The `~draft` suffix marks an unfrozen section. On acceptance the suffix becomes a
  version tag: `~draft → ~v1`, and subsequent accepted revisions increment
  (`~v1 → ~v2`). Draft sections are linkable via SLD **while still `~draft`** — so
  traceability is established from the first implementing PR.
- **When a section's `{rev}` changes, its anchor changes**, so inbound SLD
  references naming the old rev become revision-drift signals. Per DOC-15 §6.4 the
  ISR still resolves the reference to the current section and MAY flag drift;
  **enforcing** drift (OUTDATED) is a non-goal (§1.3).

### 4.5 Catalog-row requirement (NORMATIVE)

Every spec MUST have a row in the `docs/specs/README.md` "Spec catalog" table
(`DOC-N | Spec ID(s) | Title (link) | Subsystem`). A spec file without a catalog
row is **uncataloged** and is a defect to be fixed (add the row), not a valid state.

---

## 5. Resolver contract — `intent_source` beyond `.rs` (behavior only) {#SPEC-SLD-04~draft}

**ID:** SPEC-SLD-04~draft
**Status:** Draft

DOC-15 §6.4 specifies the ISR's spec-resolution leg for Rust. This section states
the **language-agnostic** contract. **It is behavior only** — the code change is a
follow-up (§11, F1), **not** in this PR.

### 5.1 Behavior contract (WHAT)

- **Input:** a changed file (path + contents) of any registered file type (§3).
- **Output:** zero or more `SpecRef { spec_id, path, anchor }` triples — one per
  declared reference — resolved to the governing `{#SPEC-…}` section(s) in
  `docs/specs/`. A file with no `# Spec References` block yields **zero** refs.
- **Algorithm (per file):**
  1. Look up the file's extension in a **per-extension comment-syntax table**
     (§5.2). Unknown extension → no refs (the resolver never guesses).
  2. Strip the comment/docstring lead-ins for that extension to expose text.
  3. Find `# Spec References` block markers (§2.3, stripped-phrase match).
  4. Within each block, apply the reference regex (§2.2) to extract
     `(spec-ID, path, anchor)` triples.
  5. Resolve each anchor to the `{#SPEC-…}` heading in the referenced `docs/specs/`
     file. An anchor that does not equal its leading ID, or that resolves to no
     section, is **dropped with a logged warning** (declared-links-only; never
     fabricate a target).
- **Declared-links-only (NORMATIVE):** absence of a block is **not** an error and
  never yields a ref. The resolver never infers linkage from file names or paths.
- **Reader-only (NORMATIVE):** the resolver never writes references and never
  enforces coverage. Four-status traceability remains a non-goal (§1.3, DOC-15
  §1.3).
- **Fail-open (NORMATIVE):** any parse/read failure yields **zero** refs for that
  file (logged to stderr), never a spurious ref — matching DOC-15's fail-open ISR
  posture (§4.2 there).

### 5.2 Per-extension comment-syntax table (the only language-specific part)

| Extension(s) | Line-comment | Block / docstring delimiters |
|---|---|---|
| `.rs` | `//`, `///`, `//!` | — |
| `.py` | `#` | `"""…"""`, `'''…'''` |
| `.ts`,`.tsx`,`.js`,`.mjs`,`.cjs` | `//` | `/** … */`, `/* … */` |
| `.sh`,`.bash` | `#` | — |
| `.toml`,`.yaml`,`.yml` | `#` | — |
| `.md` | — (visible section) | `<!-- … -->` (fallback form, §3.6) |

The table is the resolver's **only** language-specific surface; everything
downstream (marker, reference grammar, anchor resolution) is shared.

### 5.3 Rationale (WHY)

Isolating all language knowledge into one extension→comment-syntax table (§5.2)
keeps the resolver's core (§5.1) language-independent and makes adding a new
language a **data** change (one table row), not new parsing code. Stating the
contract now — but deferring the implementation — lets specs in any language start
declaring SLD links immediately, with the resolver catching up under F1.

---

## 6. Worked end-to-end example (non-normative)

A Python module governed by this spec's §3 anchor:

```python
"""Deployment helpers.

# Spec References
- SPEC-SLD-03~draft docs/specs/spec-linked-documentation.md#SPEC-SLD-03~draft
"""
```

The resolver (§5): extension `.py` → strip `#`/`"""` → find `Spec References` →
regex-extract `(SPEC-SLD-03~draft, docs/specs/spec-linked-documentation.md,
SPEC-SLD-03~draft)` → resolve the anchor to §3 of this spec. Same triple, same
resolution as the equivalent Rust `//! # Spec References` block — that is the whole
point of the one-grammar design (§2.4).

---

## 7. Acceptance criteria (testable)

**Conventions (§4):**
- **AC-1** A new spec assigned by scan-before-claim (§4.1) takes the next integer
  above the highest `DOC-N` found anywhere under `docs/` (catalog + self-labeled),
  and adds a catalog row (§4.5).
- **AC-2** A spec's header is a visible bold-field block (§4.2) with all required
  fields; no YAML frontmatter is present.
- **AC-3** Every governed section carries a `{#SPEC-…}` anchor equal to its ID (§4.3).

**Grammar & idioms (§2, §3):**
- **AC-4** The reference regex (§2.2) extracts the same `(ID, path, anchor)` triple
  from the Markdown-link form and the bare form.
- **AC-5** A `# Spec References` block is recognized in each file type of §3 (Rust,
  Python, TS/JS, shell, TOML/YAML, Markdown) via the stripped-phrase marker (§2.3).
- **AC-6** A Markdown document with a visible `## Spec References` section resolves;
  the HTML-comment fallback (§3.6) also resolves; a document with neither yields no ref.
- **AC-7** An existing Rust `//! # Spec References` block resolves **unchanged**
  (no regression — G2).

**Resolver (§5) — verified once F1 lands:**
- **AC-8** A file whose extension is not in §5.2 yields zero refs.
- **AC-9** A reference whose anchor ≠ its leading ID, or whose anchor resolves to no
  section, is dropped with a warning (declared-links-only).
- **AC-10** A file with no block yields zero refs and is not an error (fail-open).

---

## 8. Non-goals

- Four-status traceability enforcement (COVERED/UNCOVERED/ORPHANED/OUTDATED) — the
  ISR reads links; it does not gate on coverage (carried from DOC-15 §1.3).
- Implementing the resolver extension in Rust (this PR is docs-only; F1).
- Auto-inserting or auto-rewriting `# Spec References` blocks into any source.
- A CI SLD link-linter / dead-anchor check (a possible future follow-up, not
  normative here).
- Per-symbol spec-coverage metrics or a traceability matrix.
- Renumbering the DOC-28-colliding file in place (fixed by follow-up F3).
- Governing *which* code must be spec-linked — SLD defines *how* to link when an
  author chooses to; it does not mandate coverage.

---

## 9. Authoring skills — subsuming the dangling `spec-authoring` / `spec-linked-docs` {#SPEC-SLD-05~draft}

**ID:** SPEC-SLD-05~draft
**Status:** Draft

DOC-15 (`intent-conformance.md:13`, §2.4, §6.4) cites two skills that **do not
exist** anywhere (`.claude/skills/`, `crates/trusty-mpm/src/assets/`,
`~/.trusty-mpm/`). This spec is the **normative source** for both of their
responsibilities; the skills, when authored (§11, F2), are thin operational wrappers
that point back here.

- **`spec-authoring` — responsibilities (now normative in §4):** how to create a
  spec — scan-before-claim DOC-N assignment (§4.1), the header field block (§4.2),
  the `SPEC-{SUBSYSTEM}-{NN}~{rev}` ID grammar and `{#SPEC-…}` anchors (§4.3), the
  status lifecycle and rev-suffix semantics (§4.4), and the catalog-row requirement
  (§4.5). An author following §4 needs no separate skill to write a conformant spec;
  the F2 skill is a checklist/entry-point that defers to §4.
- **`spec-linked-docs` (SLD) — responsibilities (now normative in §2, §3, §5):** how
  to *link* a source artifact to a spec — the canonical reference grammar (§2), the
  per-language `# Spec References` idioms (§3, including the visible Markdown form),
  and the resolver's declared-links-only reading contract (§5). The F2 skill is a
  per-language cheat-sheet that defers to §2/§3.

**Contract for the follow-up skills (F2):** each skill's SKILL.md MUST cite this
spec (`DOC-38`) as its normative source and MUST NOT restate normative rules — it
links to the section, so there is one source of truth and the skills cannot drift
from the spec. DOC-15's dangling citations SHOULD be updated to point at DOC-38
(and, once they exist, the skills) — tracked as F4.

---

## 10. README changes (this PR)

- **Add the catalog row** for DOC-38 in `docs/specs/README.md`.
- **Rewrite the §"Spec-Linked Documentation (SLD)" section** to a short summary that
  names DOC-38 as canonical and links to it, instead of restating the Rust-only
  rustdoc convention as if it were the whole of SLD.
- **Fix the stale "next free DOC-N" note** in the DOC-28-collision catalog note:
  after the scan, DOC-34 is assigned (`managed-session-config-dir.md`), DOC-35/36
  are cataloged, DOC-37 is self-labeled (`trusty-search-managed-repo-awareness.md`,
  uncataloged), and **DOC-38 is claimed by this spec** — so the next free number for
  the DOC-28 renumber follow-up is **DOC-39**.

---

## 11. Follow-ups / child issues (for the PM to file)

Umbrella: **epic: spec-linked documentation, first-class & language-agnostic**
(issue to be linked by the PM).

| ID | Title | Scope | Deps | Effort |
|----|-------|-------|------|--------|
| **F1** | Extend `intent_source` spec-resolver beyond `.rs` | Implement §5: per-extension comment-syntax table (§5.2), stripped-phrase marker (§2.3), unified reference regex (§2.2), anchor resolution + declared-links-only/fail-open. AC-4..AC-10. | DOC-15 ISR (`intent_source`) exists | **M** |
| **F2** | Author the `spec-authoring` + `spec-linked-docs` skills | Two SKILL.md files that cite DOC-38 as normative source and defer to §4 (authoring) / §2–§3 (linkage); no restated rules (§9). | DOC-38 (this spec) | **S** |
| **F3** | Fix the DOC-28 self-label collision | Assign `mpm-cutover-resume-native-optimization.md` the next free number (**DOC-39** per §10) and add a catalog row; leave canonical DOC-28 untouched (§4.1). | scan (§4.1) | **S** |
| **F4** | Update DOC-15's dangling skill citations | Repoint `intent-conformance.md:13`, §2.4, §6.4 from the non-existent `spec-linked-docs`/`spec-authoring` to DOC-38 (and the F2 skills once they exist). | F2 | **S** |
| **F5** | Retrofit existing non-Rust source with SLD blocks (opt-in, incremental) | Where a non-Rust file is clearly governed by a spec, add a `# Spec References` block in its native idiom (§3). Declared-links-only — no bulk auto-insertion. | F1 | **M** (incremental) |

**Suggested order:** F1 ∥ F2 → F3 → F4 → F5 (F5 continuous thereafter).

---

## 12. Change log

- **2026-07-16** — Initial draft (DOC-38, `SPEC-SLD-01~draft` … `SPEC-SLD-05~draft`).
  Promoted SLD from README prose to a first-class, language-agnostic spec per Bob's
  directive ("SLD converted to a first class spec that we use for ANY language or
  even markdown"). Defined the one-grammar `(spec-ID, path, anchor)` reference (§2),
  the per-language `# Spec References` idioms incl. the visible `## Spec References`
  Markdown form (§3), the promoted spec-document conventions (§4), the resolver
  behavior contract for extending `intent_source` beyond `.rs` (§5, code as
  follow-up F1), the authoring-skills contract subsuming the dangling
  `spec-authoring`/`spec-linked-docs` skills (§9), and the F1–F5 follow-up
  breakdown. Grounded in `docs/specs/README.md` (SLD/catalog prose) and DOC-15
  (`intent-conformance.md` §1.3/§2.4/§6.4 — reader-only ISR, no four-status
  traceability).
