---
name: tm-adr
description: Architecture Decision Records (ADRs) — create, supersede and amend decision records under docs/adr/ with consistency vetting; bootstrap a project's ADR setup (init) and report whether its check-script copy matches the deployed skill (status)
user-invocable: true
metadata:
  version: "2.1.0"
category: documentation
tags: [documentation, architecture, adr, governance]
effort: medium
---

# /tm-adr

An Architecture Decision Record (ADR) captures *why* a significant, costly-to-reverse
decision was made, not only what was decided. ADRs are a first-class documentation
artifact. A decision that shapes the system is recorded; there is no informal path.

The standard — when to write an ADR, metadata, lifecycle, numbering, vetting,
governance — is [references/adr-standard.md](references/adr-standard.md). It is the
single source and lives only in this skill; init never copies it into a project.

`docs/adr/` is the one location for every step — create, supersede, amend, numbering,
init and status. No flag, environment variable or setting moves it.

## Modes

| Invocation | Does |
|---|---|
| `/tm-adr` | Create, supersede or amend an ADR in a project that already has `docs/adr/` |
| `/tm-adr init` | Bootstrap `docs/adr/` in a project with no ADR setup, as one PR |
| `/tm-adr status` | Report whether each project copy of the check script matches the deployed skill |

## Read the Project's Conventions First

Before creating, superseding or amending an ADR, read the project's `docs/adr/README.md`
for project conventions: metadata exemptions, extra decision directories, the check
command the project's CI runs. Where the README and this skill disagree, the README
wins for that project.

## When to Write an ADR

Write an ADR only when all three hold: the decision is architecturally significant, it
is costly to reverse, and its rationale is not obvious from the code. Full criteria and
examples: [references/adr-standard.md](references/adr-standard.md), "When to Write an
ADR". If unsure, ask: would a contributor in six months need to know *why*? If no, skip.

## Location and Naming

One file per decision: `docs/adr/NNNN-kebab-title.md` — four digits, zero-padded,
lowercase kebab-case. `docs/adr/INDEX.md` carries one row per ADR and is the vetting
surface. `docs/adr/template.md` is the file to copy.

## Numbering — Claim, Then Populate

Several ADRs can be in flight at once. Two authors each guessing "the next number" from
a stale checkout is how two ADRs land on one number.

1. **List the default branch, not the working tree.**
   ```bash
   git fetch origin
   git ls-tree -r --name-only origin/<default-branch> -- docs/adr/ | grep -E '/[0-9]{4}-' | sort | tail -5
   ```
   Also check open PRs and in-flight branches that touch `docs/adr/`; the default
   branch alone does not show a number claimed but not merged.
2. **Create a stub and stage it at once.** Copy `docs/adr/template.md` to
   `docs/adr/NNNN-your-title.md` with at least the title and `Status:` line, then
   `git add docs/adr/NNNN-your-title.md`. An untracked stub reserves nothing: a peer
   running the same listing does not see it. Staging is what claims the number.
3. **Then populate** the Context, Decision, Consequences and Related Decisions sections.

On a collision — your number or another author's is taken — report it and pick another.
Never renumber another author's ADR.

## Status Lifecycle

`Proposed` → `Accepted`, or `Rejected`. An accepted ADR is later marked
`Superseded by [MMMM](MMMM-title.md)` or `Amended by [MMMM](MMMM-title.md)`; the status
value is a link. Update the old ADR's status; never delete or rewrite its history.
Meanings: [references/adr-standard.md](references/adr-standard.md), "Status Lifecycle".

## Workflow

### Creating an ADR

1. Claim the number as above; copy `docs/adr/template.md`.
2. Fill in the Context, Decision and Consequences sections. Set Status to `Proposed`.
3. Fill in the Related Decisions section: sweep `docs/adr/INDEX.md` and the prior ADRs,
   record one verdict (`Consistent`, `Extends`, `Supersedes`, `Conflict(resolved)`) per
   affected prior decision. An empty Related Decisions section blocks acceptance.
4. Add the INDEX row with the same status as the file.
5. Run the check: `bash docs/adr/check_adr.sh` (bash 4+) from the project root.
6. Open the PR. The reviewer checks that the vetting is complete and names no silent
   contradiction. Once agreed, set Status to `Accepted` in the file and the INDEX row.
7. Commit message: `docs: ADR-NNNN <title> — vetting complete; <one-line summary>`.

### Superseding an ADR

1. Create `docs/adr/MMMM-new-approach.md` with Status `Accepted`; its Related Decisions
   section records **Supersedes ADR-NNNN**.
2. Set the old ADR's Status to `Superseded by [MMMM](MMMM-new-approach.md)`; update both
   INDEX rows.
3. Commit both files together: `docs: ADR-MMMM supersedes ADR-NNNN (<reason>)`.

### Amending an ADR

1. Create `docs/adr/MMMM-refinement.md` with Status `Accepted`; its Related Decisions
   section records **Amends ADR-NNNN**.
2. Set the old ADR's Status to `Amended by [MMMM](MMMM-refinement.md)`; a second
   amendment appends `, [PPPP](PPPP-title.md)`. Update both INDEX rows.
3. Commit both files together: `docs: ADR-MMMM amends ADR-NNNN (<reason>)`.

## The Check Script

`docs/adr/check_adr.sh` is a project copy of this skill's `scripts/check_adr.sh`. It
needs bash 4+ and a sha256 tool (`shasum`, `sha256sum` or `openssl`). Run it from
anywhere inside the checkout; the project root is the git toplevel.

| Flag | Env mirror | Effect |
|---|---|---|
| `--extra-dirs <glob>` | `ADR_CHECK_EXTRA_DIRS` | Each matched directory gets only the README-listing check |
| `--require-metadata-from <NNNN>` | `ADR_CHECK_REQUIRE_METADATA_FROM` | Metadata fields and vetting are required from this number on |
| `--no-index` | `ADR_CHECK_NO_INDEX=1` | Skip the INDEX checks |

A flag wins over its env mirror. Exit 0 consistent, 1 violations, 2 usage or no
`docs/adr/`, 3 no sha256 tool. The project's README names the flags its CI passes.

## Init Mode — `/tm-adr init`

Init runs only on an explicit user request. It is never invoked by `tm-init`, and never
as a side effect of another mode.

**Source.** The only source path is the base directory injected at skill load (`<base>`,
the `Base directory for this skill:` line). Never copy from a checkout, a cache or another
project. Init refuses with a named reason when `<base>/scripts/check_adr.sh` is absent:
the deployed skill is incomplete, and a re-typed script would not be byte-identical.

**Detection.** Init writes nothing when the project already has an ADR setup. Each
marker below ends the run; the output names the marker it found.

- An ADR directory other than `docs/adr/` — `adr/`, `decisions/`,
  `architecture-decisions/` — stop and name the marker.
- Any `NNNN-*.md` containing `## Decision`, or any `NNNN-*.md` already under
  `docs/adr/` — stop and name the marker.
- A `.adr-dir` file — stop and name the marker.
- A `.log4brains.yml` file — stop and name the marker.
- An ADR mention in the project's CLAUDE.md — stop and name the marker.

`docs/adr/` itself is never a marker. An existing `docs/adr/` with no `NNNN-*.md` is
filled in: files already present are kept byte-identical and named in the output,
missing files are written. Adopting a project that already has ADRs is out of scope.

**What init writes.** `docs/adr/README.md`, `template.md`, `INDEX.md` (one row, from
[references/INDEX.md](references/INDEX.md)), `0001-record-architecture-decisions.md`
(Accepted, dated the day of the run, from
[references/0001-record-architecture-decisions.md](references/0001-record-architecture-decisions.md)),
and `check_adr.sh` copied with `cp` and proven with `cmp`, mode 100755. A `.gitattributes`
`eol=lf` line for the script. When `.github/workflows/` exists,
`.github/workflows/adr-check.yml` rendered from
[references/adr-check.yml](references/adr-check.yml) to the project's default branch;
otherwise a run-it note in the README and in the PR body. Init stages what it writes and
does not commit.

**Delivery — one PR.** The dispatched agent runs one command in a worktree:

```bash
bash <base>/scripts/adr_init.sh
```

It then commits the staged files in that worktree. `version-control`, not the dispatched
agent, pushes and opens the PR. The PR body carries the file list and the `cmp` result.

**Worktree rule.** When `.trusty-mpm.toml` sets `agent_worktree = false`, dispatch the
init agent with `isolation: "worktree"`. When no worktree can be provided, stop with this
reason, verbatim: tm treats `.sh` files as source code and blocks writing or committing
them in the main checkout (ADR-0044); init must run in a worktree.

**CLAUDE.md pointer.** Init never edits CLAUDE.md. After the init PR, offer a one-line
pointer to `docs/adr/README.md`; write it only after a second, separate confirmation from
the user, and deliver it as its own PR through the same chain.

## Status Mode — `/tm-adr status`

From inside the project checkout, run:

```bash
bash <base>/scripts/adr_status.sh
```

Copies are located by `git grep -l '^# trusty-adr-check'` over `*.sh` and `*.yml`, and
each copy is reported on its own line. The comparison is header-only: the copy's
`adr-check-version` against the skill file of the same basename, and the copy's
`body-sha256` against its own body. A diff is shown only on request
(`diff <copy> <base>/scripts/check_adr.sh`).

| Outcome | Meaning | Action |
|---|---|---|
| `up to date` | Same version, pristine body | None |
| `update available` | Older version, pristine body | Offer one PR that replaces the copy, through the init delivery chain |
| `locally edited` | Body differs from its header hash | Never overwritten; report the path |
| `tm behind` | Copy is newer than the deployed skill | Nothing written; update tm's content |
| `unmanaged` | No `# trusty-adr-check` header at a known path | Report; the project maintains the file itself |
| `source` | The copy is the skill's own file under `content/skills/tm-adr/` | Never offered an update PR |

The run exits 0 only when every copy is `up to date` or `source`. An update PR follows
the init delivery chain: one command in a worktree, `version-control` pushes and opens
the PR.

## References

- [references/adr-standard.md](references/adr-standard.md) — the standard (skill-only)
- [references/template.md](references/template.md) — the template init copies to `docs/adr/template.md`
- [references/README.md](references/README.md) — the README init copies, a summary of the standard for readers without tm
- `docs/adr/INDEX.md` — the project's index of all ADRs, the vetting surface
- Michael Nygard, "Documenting Architecture Decisions" (2011)
