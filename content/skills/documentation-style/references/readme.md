# README Documentation

> **Part of**: [Documentation Style](../SKILL.md)
> **Category**: documentation
> **Reading Level**: Beginner

## Purpose

A README is entry-point orientation — for a human deciding whether to use
this project, and for an agent's first pass over a repo or crate before it
opens anything else. It answers "why would I use this, how do I install it,
how do I use it" — not the full API reference.

## Required Elements

Blending `standard-readme` conventions with a Diátaxis lens (README = mostly
*tutorial*, plus a *reference* pointer — never the full reference inline):

- A short description (ideally under 120 characters) stating what the
  project does.
- **Installation** — copy-pasteable, no missing prerequisites assumed.
- **Quick Start / usage example** — a runnable snippet before any prose.
- A **Configuration** table (option, type, default, description) when the
  project has runtime configuration.
- An **API reference pointer** — link out, don't inline the full surface.
- **Contributing** and **License** sections.

## Spec Link-Back Rule

A README is Markdown, so where the project uses spec-linked documentation, it
declares references canonically via `spec_refs:` frontmatter at the top of
the file — for example, a crate README pointing at the spec document
governing that crate. An optional informative `## Spec References` visible
section may echo the same link for readability on a code-hosting site; where
the two disagree, frontmatter wins. Omit both entirely when no spec governs
the README's subject.

## Agentic Considerations

- **One concept per section**, with consistent H2/H3 hierarchy, so a
  chunk-retrieval or RAG agent can pull a complete, self-contained unit
  instead of a fragment that needs surrounding context to make sense.
- **Quick-start code before prose.** An agent triaging "how do I use this"
  should find a runnable example within the first screen, not after several
  paragraphs of background.
- Consider an `llms.txt`-style summary for large repos, so an agent doing
  repo-level triage doesn't have to read every README to build an inventory.

## Anti-Patterns

- **Burying install/usage under marketing prose.** Lead with what the reader
  needs to act, not why the project is great.
- **A wall of text with no headers** — retrieval-hostile for both humans
  skimming and agents chunk-retrieving.
- **Duplicating the full API reference inline** instead of linking to it —
  guarantees the two copies drift.
- **Letting the README drift from the spec it summarizes.** Without a
  `spec_refs:` link, there is no mechanical way to catch this drift — that is
  precisely the gap the frontmatter link closes.
