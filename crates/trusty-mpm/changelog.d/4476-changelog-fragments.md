Changed

- per-PR changelog entries are now fragment files, not edits to a shared
  `## [Unreleased]` section (closes [#4476](https://github.com/bobmatnyc/trusty-tools/issues/4476))
  - Every PR that changes `crates/<crate>/src/**` adds
    `crates/<crate>/changelog.d/<issue-or-pr-number>-<slug>.md` — line 1 the
    category, the rest the bullet. Distinct filenames per PR mean git never sees
    a conflict; the shared section guaranteed one. Five concurrent trusty-mpm PRs
    on 2026-07-31 each wrote into `## [Unreleased]` and every merge forced the
    next to rebase and hand-resolve it.
  - `scripts/assemble-changelog.sh` folds a crate's fragments into a
    `## [<version>]` section at release time and deletes them in the same
    operation; `scripts/bump-version.sh` calls it (with a `--check` pre-flight
    before `Cargo.toml` is touched, so a changelog problem no longer leaves the
    repo half-bumped).
  - New CI gate `scripts/check_changelog_fragment.sh` fails a PR that changes
    crate source without recording it. The rule was review-gate-only before.
  - `scripts/generate-changelog.sh` is deleted. Its `git cliff --unreleased
    --prepend` blindly stacked a second `## [Unreleased]` heading — the defect
    [#2793](https://github.com/bobmatnyc/trusty-tools/issues/2793) tracks — so
    the `bump-version.sh` stopgap guard for that is gone too. `cliff.toml`
    remains, scoped to the GitHub Release body only.
  - The whole workspace is migrated in the same change: 22 crates' pending
    `## [Unreleased]` sections became fragments, 4 stale buried `[Unreleased]`
    headings left by #2793 were removed without moving their text, and
    `trusty-kb`/`trusty-bm25-daemon` gained the `---` separator the assembler
    needs. Every crate's `--check` pre-flight passes; word-level conservation
    confirms nothing was dropped.
  - An empty `changelog.d/` is explicitly NOT an error — it is the steady state
    after a release. A tracked `changelog.d/README.md` keeps the directory
    present so the next PR still sees a fragments project.
  - The gate asks the real assembler whether it would accept a crate's
    fragments rather than checking that a file exists, so a 0-byte, bodyless,
    mis-categorised or nested fragment fails in the PR that wrote it instead of
    silently vanishing from the release. `Removed` and `Security` join the
    category set, which the hand-written sections already used.
