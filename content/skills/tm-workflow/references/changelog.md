# tm-workflow — the changelog entry

## Changelog Requirement (Before Merge)

Changelogs go stale when updating them is left to "sometime at release", so it is
part of every PR. Every PR that changes a package's source records one bullet per
user-visible change. Docs-only / CI-only PRs may skip this.

**Prefer a per-PR fragment file.** When the package has a `changelog.d/`
directory, that is where the entry goes:

```
<package>/changelog.d/<issue-or-pr-number>-<short-slug>.md

Fixed   <- line 1: Breaking|Added|Fixed|Performance|Changed|Removed|Security|Documentation

- the bullet text, in the package CHANGELOG's existing style
```

One new file per PR, named after the issue/PR number, means two concurrent PRs
never touch the same lines. A shared `## [Unreleased]` section guarantees a
conflict instead. Release time assembles the fragments into `CHANGELOG.md` and
deletes them; never hand-edit `CHANGELOG.md` in a package that uses fragments.

The file goes DIRECTLY in `changelog.d/`, never a subdirectory; a `README.md`
already sitting there is the directory's placeholder, not a fragment to copy.
One category per fragment — the first line IS the category and everything after
it belongs to it, so a second category word inside the body is a gate failure
and two categories mean two files (#7287).

If the package has no `changelog.d/`, add the bullet to `CHANGELOG.md` under the
topmost `## [Unreleased]` heading (create it if absent), matching the file's
existing style.

- A PR that changes source and lands without a matching changelog entry is a
  **review-gate failure** — the same tier as a failing build/test/lint gate.
  Treat it exactly like CB#8: block the merge, delegate back to the engineer,
  don't wave it through as "trivial."
- Self-check with `bash scripts/check_changelog_fragment.sh` before pushing —
  the same script CI and the pre-commit hook run, so a nonzero exit here is the
  same finding CI would report, just earlier.
- If the project also runs automated changelog generation at release time, check
  the project's root `CLAUDE.md` for which mechanism owns `CHANGELOG.md` before
  assuming they coexist. Two writers to one file is a defect; do not invent a
  precedence rule on the fly.
