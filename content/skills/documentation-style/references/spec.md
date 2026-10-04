# Spec Documentation

> **Part of**: [Documentation Style](../SKILL.md)
> **Category**: documentation
> **Reading Level**: Intermediate

## Purpose

A spec is a behavior-contract document governing a subsystem. It is the
**target** of spec-link-back references from code, config, and other
Markdown — never a source pointing somewhere else for its own behavior
(though it may point at a spec it *builds on*; see below).

## Required Elements

Where a project uses DOC-38-style spec-linked documentation, a conformant
spec carries (see `docs/specs/spec-linked-documentation.md` §4 for the
normative rules — this reference restates none of them):

- A title line: `# DOC-N — Title`, where `N` is assigned by **scanning the
  spec catalog for the next free number**, never by trusting a stale "next
  free" note.
- A bold-field header block: **Status**, **Subsystem**, **Owner**,
  **Last-updated**, **Spec ID** (required); **Epic**, **Builds on**,
  **Cross-ref** (optional).
- Every governed section carries an anchor in the form
  `{#SPEC-{SUBSYSTEM}-{NN}~{rev}}`.
- A catalog row in the repository's spec index (e.g. `docs/specs/README.md`).
- A `Draft → Accepted → Superseded` lifecycle per section, expressed through
  the `~rev` suffix (`~draft → ~v1 → ~v2`, or a named tag like `~approved`).

If your project uses a different spec convention (or none), adapt the shape
above — the point is a stable ID, an anchor, and a catalog entry, however
your project spells them.

## Spec Link-Back Rule

Specs generally sit at the top of the reference graph and don't link back to
something above them. A spec **may** carry `spec_refs:` frontmatter pointing
at a spec it explicitly builds on or extends — this is the same mechanism
code uses, applied to the spec document itself. Numbering is always
scan-before-claim; never trust a catalog's cached "next free number" hint,
which can and does drift out of sync with reality.

## Agentic Considerations

- **`{NN}` is opaque.** Don't infer document structure or reading order from
  the numeric ID sequence — IDs are assigned by scan order, not narrative
  order.
- **`{rev}` is an open token**, not a closed enum of `draft`/`v1`/`v2`. A
  resolver (human or agent) must not reject an unrecognized `~<token>` — new
  named tags (e.g. `~approved`) are valid.
- **One decision per record, immutable.** Architecture Decision Records
  (Nygard format: Title / Context / Decision / Status / Consequences) are the
  reference shape for the "decision" subtype of spec — amend by superseding a
  record with a new one, never by editing history in place.

## Anti-Patterns

- **Reusing a colliding ID** instead of scanning for the next free number —
  this has caused real numbering collisions in this repo's own spec catalog.
- **Metadata leaking into `spec_refs:` frontmatter.** Frontmatter is
  references-only; it never duplicates the bold-field header block.
- **Trusting a catalog's cached hint** over an actual scan of existing spec
  files — the hint is documentation, not ground truth.
