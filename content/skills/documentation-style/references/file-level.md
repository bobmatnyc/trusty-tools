# File-Level Documentation

> **Part of**: [Documentation Style](../SKILL.md)
> **Category**: documentation
> **Reading Level**: Beginner

## Purpose

File-level documentation states what this file is for and *why it exists as
a separate unit* — module-level orientation, not a restatement of its
contents. It is the highest-value doc for an agent doing file-level triage:
front-load "why this file exists" so relevance can be judged before opening
the body.

## Required Elements

- Rust: the `//!` inner-doc idiom at the top of the module — this repo's
  existing convention, unchanged by spec-linked documentation (per DOC-38
  §3.1, where a project uses it).
- Non-Rust languages: the equivalent module-header docstring/comment form —
  a one-sentence abstract (the `\brief`-equivalent) followed by longer
  discussion in separate paragraphs, per the Doxygen/LLVM convention.
- Avoid restating what's already inferable from the filename or the public
  API surface — the file header earns its cost by adding context the
  signature list can't.

## Spec Link-Back Rule

Where a project uses spec-linked documentation, a file whose behavior is
spec-governed carries a `# Spec References` block (inline idiom) immediately
below the file-level Why/What prose — introduced by a marker line whose
stripped text is exactly "Spec References". Scoping follows a
nearest-preceding-marker rule: a file may carry zero, one, or (rarely) both a
module-level block and additional per-item blocks further down. Omit the
block entirely when nothing in the file is spec-governed.

## Agentic Considerations

This is the doc an agent reads *first* when triaging a file it hasn't opened
yet. A strong file header lets an agent decide relevance — "is this the file
I need to touch?" — without reading the body. Two things make this decision
fast: a clear *why this file is separate* statement, and, when present, its
spec linkage (so the agent knows immediately whether a spec constrains any
change here).

## Anti-Patterns

- **Headers that restate the filename.** `//! This file contains the User
  struct.` tells the reader nothing they didn't already know from the path.
- **Excessively long headers that duplicate in-body docs.** If the file
  header repeats what each item's own doc comment already says, one of the
  two will drift; keep the header to file-scoped orientation only.
