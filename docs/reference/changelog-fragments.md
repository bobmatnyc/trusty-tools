# Per-PR Changelog Fragments — Repo Mechanics

> **The requirement ("every PR touching a crate's `src/**` adds a fragment") is
> in [`CLAUDE.md`](../../CLAUDE.md)**, and the fragment file format is in the
> `tm-workflow` skill — call `Skill(skill="tm-workflow")` for it. This
> page is the trusty-tools-specific assembler and CI-gate detail: consult it
> when writing a fragment or debugging a rejected one.

Introduced by issue #4476, superseding the shared-`## [Unreleased]` convention.

## File Location and Name

```
crates/<crate>/changelog.d/<issue-or-pr-number>-<short-slug>.md
```

```
Fixed

- pm_guard no longer scans quoted content (closes [#2741](https://github.com/bobmatnyc/trusty-tools/issues/2741))
  - indented sub-bullets are preserved verbatim
```

- **Line 1 is the category:** `Breaking` | `Added` | `Fixed` | `Performance` |
  `Changed` | `Removed` | `Security` | `Documentation` (the same groups the
  changelogs already use).
- **Everything after it is the bullet text**, copied through verbatim. Match the
  crate CHANGELOG's existing style.
- **The file must sit directly in `changelog.d/`.** A nested one
  (`changelog.d/sub/…`) is rejected at release time; `changelog.d/README.md` is
  the tracked directory placeholder and is never treated as a fragment.
- **The filename's leading number is what makes this collision-free.** Two
  concurrent PRs add two differently-named files, so git never sees a conflict.
  That is the entire point: on 2026-07-31 five concurrent trusty-mpm PRs
  (#4463–#4475) each wrote a bullet into the shared `## [Unreleased]` section and
  every merge forced the next PR to rebase and hand-resolve it (#4399 burned
  three such rounds).

## One Fragment Carries One Category

Stacking several into one file — a bare `Changed` line below a `Removed` line 1
— is rejected by the assembler and the CI gate, because "verbatim" means the
second category would render as body text under the first one's heading. That
shipped once (the 1.3.3 `4286-retire-trusty-mpm-override-files.md` fragment put
four categories under `### Removed`). Split it: same number, different slug.

The heading form (`## Changed`) counts as a second category too; anything inside
a code fence does not, so a fragment may show example output freely.

## Preview the Pending Set

```bash
bash scripts/assemble-changelog.sh <crate-dir> --stdout
```

## Fragments Are the Source of Truth

`CHANGELOG.md` carries released version sections only — no `## [Unreleased]`
heading between releases. At release time `scripts/bump-version.sh` calls
`scripts/assemble-changelog.sh <crate-dir> <version>`, which groups the crate's
fragments by category, writes one `## [<version>] — <date>` section, and deletes
the consumed fragments in the same operation.

A bump that is not a release cut must not do that. Pass
`scripts/bump-version.sh <crate-dir> <level> --no-changelog` when the bump rides
along in the PR carrying its source change: the fragments there belong to work
still in flight, and consuming them makes this gate fail the PR for lacking a
fragment the bump deleted (issue #5674).

## Recovering a Stale Section (`--merge`)

Assembling is not coupled to publishing, so a `## [<version>]` section can exist
for a cut that never shipped. PR #4824 wrote `## [1.3.5]` into trusty-mpm and
`## [0.5.0]` into trusty-agents-common that way, consuming 40 fragments; 77 more
accumulated behind those sections over the next six days, and the assembler
refused every subsequent run.

The assembler still refuses by default — merging rewrites a section that may
already be published and tagged, and the script cannot tell a phantom cut from a
shipped one. What it now does is name every stranded fragment in the refusal and
offer the two ways out (#5298):

```bash
# 1. fold the pending fragments into the existing section
bash scripts/assemble-changelog.sh <crate-dir> <version> --merge

# 2. or cut the next version instead, leaving the stale section as history
```

`--merge` appends to the `### <Category>` subsections the section already has,
inserts missing categories in the canonical order, deletes the consumed
fragments in the same operation, and exits nonzero without touching anything if
any category could not be placed. It refuses outright when there is no
`## [<version>]` section to merge into. Hand-editing `CHANGELOG.md` remains
banned either way.

## The Release Window (`--merge` in the Same PR)

Between a release cut merging and its tag being pushed, a crate's
`## [<version>]` section already exists. A source fix landing in that gap cannot
just add a fragment: `scripts/check-changelog-assembled.sh` — preflight CHECK 9,
and the merge-time half in `changelog-fragment.yml` — fails on any fragment left
unconsumed, so the fix would block the publish it is meant to unblock. Folding it
in with `--merge` then deleted the fragment, and the per-PR fragment gate read
that as an omission. Both gates were right and the PR could satisfy neither
(issue #6695; observed on `fix/prepublish-doc-links-20260902`, `ffe03c23c`).

Write the fragment as usual, then fold it in, in the same PR:

```bash
bash scripts/assemble-changelog.sh <crate-dir> <version> --merge
```

`check_changelog_fragment.sh` accepts that as the crate's record when all four
hold:

- the `## [<version>]` section existed already at the merge base, and at HEAD it
  carries at least one bullet it did not carry there;
- it lost none, and none of the bullets it already carried was edited. A fold
  only adds — rewording an existing bullet is not a record of anything;
- `<version>` is what the crate's `Cargo.toml` ships right now;
- no `<package>-v<version>` tag exists yet — after the tag the section is
  released history, and a bullet written into it back-dates the change;
- `check-changelog-assembled.sh <crate> <version>` exits 0. The fragment gate
  asks that gate rather than re-deriving what "assembled" means, which is what
  keeps the two verdicts in step.

Outside that window nothing changes: a hand-written `CHANGELOG.md` bullet is
still not a record, and the fragment is still the rule.
`scripts/check_changelog_release_window_selftest.sh` holds both directions.

## git-cliff No Longer Touches `CHANGELOG.md`

`scripts/generate-changelog.sh` is deleted; it ran `git cliff --unreleased
--prepend`, which blindly stacked a fresh `## [Unreleased]` on top of the
hand-written one — the defect #2793 tracks and the reason `bump-version.sh` used
to carry a duplicate-heading stopgap. Both are gone: there is exactly ONE
mechanism that writes a crate changelog, and it is the assembler. `cliff.toml`
stays, scoped to rendering the **GitHub Release body** in
`.github/workflows/release.yml` (`--latest --strip all`), which never writes
`CHANGELOG.md`.

## Enforcement

A PR that changes crate source and lands with no fragment is a **review-gate
failure**, the same tier as a failing `cargo test` / `cargo clippy` gate — and it
is also a CI failure (`.github/workflows/changelog-fragment.yml` →
`scripts/check_changelog_fragment.sh`). No "trivial change" exception.

Docs-only, CI-only, test-only and `testdata/` PRs may skip the fragment.

## Transitional Note

PRs opened before #4476 landed wrote into the shared `## [Unreleased]` section
and were not converted. The gate accepts a `CHANGELOG.md` edit as evidence so
they stay green, and the assembler refuses to run while a leftover
`## [Unreleased]` heading survives — fold those bullets into the section being
cut at the next release of that crate.
