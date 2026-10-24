# The ADR Standard

> **Part of**: [tm-adr](../SKILL.md)
> **Category**: documentation

This file is the single source for the ADR standard the `tm-adr` skill applies. It
stays in the skill; init never copies it into a project. A project's `docs/adr/README.md`
summarises it for readers without tm and adds the project's own rules.

An Architecture Decision Record (ADR) is one file per decision under `docs/adr/`. It
records the context that forced a decision, the decision itself, its consequences, and
how it was vetted against the decisions already in force. The format is Nygard's
(Title, Status, Context, Decision, Consequences) plus a metadata block and a Related
Decisions section.

## When to Write an ADR

Write an ADR when all three conditions hold:

1. **Architecturally significant** — the decision shapes how major parts of the system
   are structured, constrains future options, or crosses a subsystem boundary.
2. **Costly to reverse** — undoing it later needs substantial rework, a migration, or
   coordination across teams or components.
3. **Not obvious from the code** — the rationale is not apparent from reading the
   implementation.

Write one for: an IPC protocol between services, a credential-routing model, a
toolchain or language-version floor, a persistence strategy, a service boundary, the
deprecation of a foundational component.

Do not write one for: a bug fix, a small feature, a style or lint setting, a library
version bump, or anything a code-review comment covers.

ADRs are rare and formal. A typical quarter yields a few, not dozens. When a decision
meets the bar, recording it is required, not optional.

## File Location and Naming

`docs/adr/NNNN-kebab-title.md`: four digits, zero-padded, lowercase kebab-case, unique
and continuous within the directory. The first line is `# NNNN. Title`, where `NNNN`
equals the filename's number. The title is short and imperative: "Use X for Y".

The location is fixed. No project setting, flag or environment variable moves it.

## Metadata Fields

Every ADR opens with this block, before the `## Context` heading:

```markdown
# NNNN. <Short Title of Decision>

- **Status:** Proposed | Accepted | Rejected | Superseded by [MMMM](MMMM-title.md) | Amended by [MMMM](MMMM-title.md)[, [PPPP](PPPP-title.md)…]
- **Date:** YYYY-MM-DD
- **Scope:** Project-wide (or: subsystem `<name>`)
- **Reversibility Cost:** Low | Medium | High — cost to undo this choice later
- **Decision Drivers:** comma-separated forces, e.g. "toolchain floor, latency ceiling, service boundary"
- **Supersedes / Superseded by:** — (or a link per lifecycle relationship)
```

| Field | Rule |
|---|---|
| Status | One of the five values in "Status Lifecycle"; a lifecycle target is a Markdown link to the ADR file |
| Date | ISO `YYYY-MM-DD`; the day the record was written |
| Scope | Project-wide, or the named subsystem the decision binds |
| Reversibility Cost | `Low`, `Medium` or `High`, with a short reason |
| Decision Drivers | The forces that made the decision necessary |
| Supersedes / Superseded by | `—` when there is no lifecycle relationship |

Every field is non-empty. A project may exempt records written before it adopted the
block; it names the first governed number in its README and passes it to the check as
`--require-metadata-from <NNNN>`.

## Core Sections

| Section | Content |
|---|---|
| `## Context` | The forces, constraints and alternatives. Facts, not opinions. Why was a decision needed now? |
| `## Decision` | What was decided, in active voice: "We will …". One decision per ADR. |
| `## Consequences` | What becomes easier and harder. Positive, negative and neutral effects, follow-up work, known risks. |
| `## Related Decisions` | The vetting record; see "Consistency Vetting". |

Each core section is non-empty. A heading with no body fails the check.

## Status Lifecycle

| Status | Meaning |
|---|---|
| `Proposed` | Drafted, under discussion. Vetting is optional while Proposed and required before acceptance. |
| `Accepted` | Agreed and in force. All Accepted ADRs form the current decision set. |
| `Rejected` | Considered and not adopted. Kept so the same choice is not re-litigated. |
| `Superseded by [MMMM](MMMM-title.md)` | Replaced by a later ADR. The old decision is no longer in force. |
| `Amended by [MMMM](MMMM-title.md)[, …]` | Refined, not replaced, by one or more later ADRs. The old decision stays in force together with every amendment. |

Transitions: `Proposed` → `Accepted` or `Rejected`; `Accepted` → `Superseded by` or
`Amended by`. A record may land as `Superseded by` directly when it is already
superseded by the time it merges.

A superseded or amended ADR keeps its Context, Decision and Consequences as written.
Only its Status, its lifecycle field and its INDEX row change. The successor's Related
Decisions section carries the backlink (`Supersedes ADR-NNNN` or `Amends ADR-NNNN`), so
every lifecycle link resolves in both directions.

## Numbering

Sequential, zero-padded four-digit integers with no gaps. Several ADRs can be in flight
at once, so the number is claimed before the record is written:

1. **Check before choosing.** List `docs/adr/` on the remote default branch, not the
   local working tree, and check open PRs and in-flight branches for a number already
   claimed but not merged.
2. **Claim by staging.** Create the file with at least the title and `Status:` line,
   then `git add` it. An untracked stub reserves nothing: a peer running the same
   listing does not see it. The staged stub is the claim.
3. **Populate** Context, Decision, Consequences and Related Decisions.

On a collision, report it and take another number. Never renumber another author's
ADR; the two authors, or whoever coordinates them, settle it.

## Consistency Vetting

Before an ADR becomes `Accepted`, the author sweeps the decisions already in force and
records the outcome in `## Related Decisions`. The sweep covers every `Accepted` and
`Amended by` ADR in `docs/adr/INDEX.md`, plus any extra decision directories the
project's README names.

```markdown
## Related Decisions

Vetted against prior ADRs on YYYY-MM-DD:

- **ADR-NNNN (Title):** Consistent — aligns with the prior decision; no conflict.
- **ADR-NNNN (Title):** Extends — builds on the prior decision; a compatible evolution.
- **ADR-NNNN (Title):** Supersedes — replaces the prior decision; its status becomes `Superseded by` this ADR.
- **ADR-NNNN (Title):** Conflict(resolved) — contradicts the prior decision; the resolution is stated here.
```

Verdict codes:

| Verdict | Meaning | Action on the prior ADR |
|---|---|---|
| `Consistent` | Aligns; no conflict | None |
| `Extends` | Builds on it; compatible evolution | None, or `Amended by` when the prior text is refined |
| `Supersedes` | Replaces it | Status becomes `Superseded by [this](this.md)` |
| `Conflict(resolved)` | Contradicts it | State how the conflict is resolved: which decision takes precedence, or the scope in which each holds |

Outcomes:

- Every in-force ADR carries a non-empty Related Decisions section.
- When no prior decision exists or none is affected, the section reads:
  `No prior decisions to vet against.`
- An ADR that contradicts an Accepted decision either resolves the contradiction in
  Related Decisions, is reworked to remove it, or is rejected. It never lands silently.

The check enforces the section's presence and body. Whether the verdicts are *right*
is a review judgement: the reviewer reads the swept ADRs and confirms no contradiction
was missed.

## The Index

`docs/adr/INDEX.md` holds one row per ADR and is the cheap vetting surface:

```markdown
| # | Title | Status | One-line decision | Scope |
|---|---|---|---|---|
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted | Decisions are recorded as ADRs under docs/adr/. | Project-wide |
```

The row's Status equals the file's Status with links reduced to their text
(`Superseded by 0007`). Every ADR file has exactly one row; every row has a file.

## Mechanical Checks

`docs/adr/check_adr.sh` runs in the project's CI and before every ADR PR. It enforces:

1. Naming — `NNNN-lowercase-kebab.md`, and a heading that self-identifies as `NNNN`.
2. Numbering — unique and continuous.
3. Core sections — Context, Decision and Consequences present and non-empty.
4. Metadata — every field non-empty and the Date ISO, from the governed number on.
5. Status grammar — one of the five values; lifecycle targets exist; a superseded
   ADR's successor carries the backlink.
6. Vetting — a non-empty Related Decisions section on every in-force ADR from the
   governed number on.
7. Index parity — one row per file, one file per row, equal status.
8. Extra directories (`--extra-dirs <glob>`) — every `NNNN-*.md` is named in that
   directory's `README.md`; nothing else is checked there.

Semantic consistency between two Accepted ADRs is not mechanically checked.

## Governance

- **Immutable once accepted.** A decision changes by a new ADR that supersedes or
  amends it, never by editing the old record's Context, Decision or Consequences.
- **Never delete an ADR.** Rejected and superseded records stay in the corpus and the
  index.
- **Acceptance is a review gate.** The reviewer confirms: imperative title, factual
  Context, unambiguous Decision, Consequences that name the costs, a complete Related
  Decisions section, bidirectional lifecycle links, and a number that follows the
  sequence.
- **Blocks acceptance:** an unresolved contradiction with an Accepted ADR; an empty or
  incomplete Related Decisions section; a numbering gap or duplicate; a missing or
  invalid Status.
- **Project rules live in the project.** Exemptions, extra decision directories and
  the CI flags are stated in that project's `docs/adr/README.md`, which an agent reads
  before any ADR step. This file states only the generic standard.
