# Architecture Decision Records (ADRs)

This directory holds the project's Architecture Decision Records. An ADR captures *why*
one architecturally significant, costly-to-reverse decision was made: the context that
forced it, the decision itself, its consequences, and how it was vetted against the
decisions already in force.

This README summarises the standard the records follow. It is project-owned: add the
project's own rules here (metadata exemptions, extra decision directories, the flags CI
passes to the check).

## Files

| File | Purpose |
|---|---|
| `NNNN-kebab-title.md` | One record per decision |
| [`INDEX.md`](INDEX.md) | One row per ADR; the vetting surface |
| [`template.md`](template.md) | Copy this to start a new record |
| [`check_adr.sh`](check_adr.sh) | The consistency check; see "Running the check" |

## When to write one

Write an ADR when all three hold: the decision is **architecturally significant**, it
is **costly to reverse**, and its rationale is **not obvious from the code**. Examples:
an IPC protocol, a credential-routing model, a toolchain floor, a persistence strategy,
a service boundary. Do not write one for a bug fix, a small feature, a lint setting or
a library version bump. When a decision meets the bar, recording it is required.

## Record shape

The first line is `# NNNN. Title` with the file's number. A metadata block follows:
Status, Date (`YYYY-MM-DD`), Scope, Reversibility Cost, Decision Drivers, and
Supersedes / Superseded by. Then four sections, each non-empty: `## Context`,
`## Decision`, `## Consequences`, `## Related Decisions`.

## Status lifecycle

| Status | Meaning |
|---|---|
| `Proposed` | Drafted, under discussion |
| `Accepted` | Agreed and in force |
| `Rejected` | Considered and not adopted; kept for the record |
| `Superseded by [MMMM](MMMM-title.md)` | Replaced by a later ADR; no longer in force |
| `Amended by [MMMM](MMMM-title.md)` | Refined by one or more later ADRs; still in force with them |

A decision changes by a new ADR that supersedes or amends the old one. The old record
keeps its Context, Decision and Consequences; only its Status, its lifecycle field and
its INDEX row change. Never delete an ADR.

## Numbering — claim, then populate

Four digits, zero-padded, continuous. Several ADRs can be in flight at once:

1. List `docs/adr/` on the remote default branch and check open PRs for a number
   already claimed.
2. Create the file with at least the title and `Status:` line and `git add` it at
   once. The staged stub is the claim; an untracked file reserves nothing.
3. Fill in the sections.

On a collision, take another number. Never renumber another author's ADR.

## Consistency vetting

Before an ADR becomes `Accepted`, sweep [`INDEX.md`](INDEX.md) and record one verdict
per affected prior decision in `## Related Decisions`:

- `Consistent` — aligns; no conflict.
- `Extends` — builds on it; a compatible evolution.
- `Supersedes` — replaces it; set the old ADR's Status to `Superseded by` this one.
- `Conflict(resolved)` — contradicts it; state how the conflict is resolved.

With no prior decision affected, write `No prior decisions to vet against.` An ADR that
contradicts an Accepted decision never lands silently: resolve it, rework it, or reject
it.

## Writing a new ADR

1. Claim a number and copy [`template.md`](template.md) to `NNNN-kebab-title.md`.
2. Fill in the metadata block, Context, Decision and Consequences. Status `Proposed`.
3. Fill in Related Decisions after the sweep.
4. Add the INDEX row with the same Status as the file.
5. Run the check (below); open the PR.
6. When agreed, set Status to `Accepted` in the file and the INDEX row.

## Running the check

```bash
bash docs/adr/check_adr.sh
```

The script needs bash 4 or newer (macOS ships 3.2: `brew install bash`) and one of
`shasum`, `sha256sum` or `openssl`. Run it from anywhere inside the checkout; it finds
the project root through git. It enforces naming, continuous numbering, non-empty core
sections, the metadata block, the status grammar, lifecycle backlinks, a non-empty
Related Decisions section on every in-force ADR, and INDEX parity.

| Flag | Env mirror | Effect |
|---|---|---|
| `--require-metadata-from <NNNN>` | `ADR_CHECK_REQUIRE_METADATA_FROM` | Metadata and vetting required from this number on; exempts older records |
| `--extra-dirs <glob>` | `ADR_CHECK_EXTRA_DIRS` | Each matched directory gets only the README-listing check |
| `--no-index` | `ADR_CHECK_NO_INDEX=1` | Skip the INDEX checks |

A flag wins over its env mirror. Exit 0 consistent, 1 violations, 2 usage or no
`docs/adr/`, 3 no sha256 tool. `bash docs/adr/check_adr.sh --verify-self` confirms the
script's body matches its header hash.

## The script is a managed copy

`check_adr.sh` opens with three header lines: `# trusty-adr-check`,
`# adr-check-version: <version>` and `# body-sha256: <hash of the body>`. Do not edit
the body; an edit makes `--verify-self` fail and marks the copy as locally edited, so it
is never updated in place. A newer version arrives as a pull request from the tool that
installed it (`/tm-adr status`), replacing the file byte for byte. A project without that
tool keeps the copy as it is.
