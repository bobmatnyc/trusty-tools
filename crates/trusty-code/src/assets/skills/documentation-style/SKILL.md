---
name: documentation-style
description: "SLD-grounded per-artifact-type documentation style guide: the four-axis Why/What/Test + Spec-References inline model, plus focused conventions for specs, READMEs, files, classes, methods/functions, and block comments. Use when writing or reviewing any documentation artifact."
user-invocable: false
version: "1.0.0"
category: agent-reference
tags: [documentation, sld, spec-linked-documentation, style-guide, agent-reference]
effort: medium
spec_refs:
  - id: SPEC-SLD-02~draft
    path: docs/specs/spec-linked-documentation.md
    anchor: SPEC-SLD-02~draft
    note: "Canonical spec-reference grammar this skill's Spec-References guidance defers to; this skill restates none of it."
---
# Documentation Style

Per-artifact-type documentation conventions, unified around one model: every
doc comment carries **Why**, **What**, and **Test** (this project's mandatory
triple where it applies), plus an optional fourth axis — a **Spec References**
link-back — when, and only when, a real spec governs the artifact.

This skill is a style guide, not a linkage standard. Where a project defines a
spec-linked-documentation (SLD) convention, this skill **defers to it and
restates none of its grammar**. In this repo that convention is DOC-38
(`docs/specs/spec-linked-documentation.md`) — read it directly for the actual
reference grammar, frontmatter schema, and per-language idioms. If your
project has no such convention, skip the Spec References axis entirely; the
Why/What/Test triple (or your project's equivalent) stands on its own.

## The Four-Axis Model

| Axis | Answers | Mandatory? |
|---|---|---|
| **Why** | What problem does this solve? What's the motivation? | Yes for non-obvious code; lighter touch OK for trivial items |
| **What** | What does it mechanically do? | Yes for non-obvious code; lighter touch OK for trivial items |
| **Test** | Where is this proven? (backtick-quoted test name(s)) | Yes for non-obvious code; lighter touch OK for trivial items |
| **Spec References** | Which spec section governs this, if any? | **Opt-in** — only when a real spec governs it |

**Proportionality principle:** Documentation effort scales with how surprising the
code is. For self-evident items (obvious one-liners, trivial getters, thin facades),
a single-line summary suffices; for API entry points and design-heavy code, the
full Why/What/Test pattern is mandatory. **Why**/**What**/**Test** stay exactly what
your project's convention defines when invoked — the proportionality amendment only
gates *when* each axis is required. **Spec References** is a fourth, *additive*
axis: a separate block within the same doc comment, introduced by its own marker,
never a replacement for the other three.

**Full pattern** (API entry point with governing spec, all axes):

```rust
/// Why: <motivation>
/// What: <mechanical description>
/// Test: `test_name_a`, `test_name_b`
///
/// # Spec References
/// - [`SPEC-FOO-01~draft`](docs/specs/foo.md#SPEC-FOO-01~draft)
pub fn my_function() { … }
```

**Lighter touch** (trivial item, one-line summary sufficient):

```rust
/// Returns the caller's working directory.
pub fn cwd(&self) -> &Path { … }
```

**Never fabricate a spec link.** Add `# Spec References` (inline) or
`spec_refs:` (Markdown frontmatter) only when a spec genuinely governs the
item. An item with no governing spec simply omits the block — that is the
correct, complete state, not an oversight to "fix" by inventing a link to
satisfy a checklist.

**When to use the full pattern:** API entry points, design-heavy code, error
contracts, safety/TCC behavior, cross-crate surfaces.
**When a one-liner suffices:** Trivial getters, obvious constructors, thin
re-exports, one-line helpers. If a competent reader's first guess is right,
one line is enough.

**Ticket-attributed changes:** When you modify a function, class, or module
*because of* a ticket, add a concise inline pointer at the change site —
`// #1234: <one-line reason>`, or `// See #1234` when the ticket title says
it all. This uses the same pointer convention described above for
defensive-reasoning paragraphs: the ticket reference is the pointer, never a
narrative. Full context stays in the ticket, not a changelog embedded in
comments.

## Per-Type Guides

Each artifact type gets a focused reference under `references/` — read the one
that matches what you're writing, not the whole set.

| Type | Reference | Covers |
|---|---|---|
| Spec (behavior-contract document) | [spec.md](references/spec.md) | Header block, numbering, anchors, lifecycle |
| README | [readme.md](references/readme.md) | Entry-point orientation for humans and agents |
| File-level | [file-level.md](references/file-level.md) | Module/file header docs (`//!`, docstrings) |
| Class/Module (struct/trait/type) | [class.md](references/class.md) | Type-level contracts, invariants, thread-safety |
| Method/Function | [method-function.md](references/method-function.md) | Callable contracts — pre/postconditions, `Test:` pointers |
| Block/Inline | [block-inline.md](references/block-inline.md) | Trailing/preceding line comments |

## Context-Economy Principles

Documentation competes for the same context window an agent uses to reason
about the task. Treat every line of doc as a cost, not a free good:

- **Say less.** State the contract; don't narrate the implementation.
- **Say it once.** Don't restate at the file level what the function-level doc
  already says, or restate what the type signature already shows.
- **Link out.** Point to the deeper reference (a `references/*.md` file, a
  spec section) instead of inlining it. Progressive disclosure keeps the
  entry point dense and the detail one hop away.

A factorial study on coding-agent instruction files found file *structure*
(length, placement, section architecture) had no detectable effect on
instruction adherence, while compliance measurably degrades as more content
accumulates within a session. Treat this as a caveat against assuming any
particular formatting trick helps, not as license to skip Why/What/Test — the
economy argument is about redundancy, not about omitting the mandatory axes.

## Anti-Patterns (ported from `api-documentation`)

- **Redundant comments** — `i = i + 1  // increment i` adds zero information;
  a *why* comment (`// skip header row`) survives refactors that change the
  *how*.
- **Outdated documentation** — a doc comment that no longer matches the code
  it describes is worse than no comment; it actively misleads.
- **Implementation, not contract** — `"""Uses bubble sort to sort users."""`
  documents *how*; `"""Returns users sorted alphabetically by name."""`
  documents *what*. Callers need the contract, not the algorithm.

## Quick Checklist

```
□ Non-obvious public APIs carry full Why/What/Test; trivial items get one-line summaries
□ Spec References added only where a spec genuinely governs the item
□ Parameters, returns, and errors documented (where non-obvious)
□ Usage example provided where the contract or behavior isn't self-evident
□ File-level doc states why the file exists, not just what's in it
□ No comment restates what the signature or a one-line summary already shows
□ Defensive-reasoning paragraphs replaced with links to issues or ADRs
□ Ticket-driven changes carry an inline pointer at the change site (`// #1234: …`)
□ Docs updated in the same change as the code they describe
```

## Related Skills

- **[api-documentation](../api-documentation/SKILL.md)** — general per-language
  docstring/JSDoc/Godoc examples this skill's `method-function.md` builds on.
- **[contract-driven-testing](../contract-driven-testing/SKILL.md)** — deriving
  tests from the preconditions/postconditions a Method/Function doc states.

## Spec References

- [`SPEC-SLD-02~draft`](../../../../../docs/specs/spec-linked-documentation.md#SPEC-SLD-02~draft) — the canonical spec-reference grammar (inline and frontmatter forms) this skill's Spec-References guidance defers to.
