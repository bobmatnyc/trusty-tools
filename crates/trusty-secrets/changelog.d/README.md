# changelog.d — per-PR changelog fragments

Every PR that changes this crate's `src/**` adds ONE file here instead of editing
`CHANGELOG.md`. Editing a shared `## [Unreleased]` section made every concurrent
PR conflict on the same lines; a per-PR filename cannot collide (issue #4476).

    crates/<crate>/changelog.d/<issue-or-pr-number>-<short-slug>.md

    Breaking | Added | Fixed | Performance | Changed | Removed | Security |
    Documentation                                                  <- line 1

    - one bullet per user-visible change, in this CHANGELOG's existing style
      - indented sub-bullets are preserved verbatim

ONE fragment carries ONE category. Only line 1 is read as a category; everything
after it is copied through verbatim, so a second one stacked into the same file
(a bare `Changed` line below a `Removed` line 1) would render as body text under
the first heading. That shipped once and is now rejected — split it into one
fragment per category, reusing the same number with a different slug. The
heading form (`## Changed`) counts too; anything inside a code fence does not,
so a fragment may show example output freely.

The leading number is what makes the name collision-free (GitHub issue and PR
numbers are unique per repo); the slug keeps two fragments for one number
distinct. The file must sit directly in this directory — a nested one is
rejected.

Preview what the next release will say:

    bash scripts/assemble-changelog.sh <crate> --stdout

`scripts/assemble-changelog.sh <crate> <version>` folds these into `CHANGELOG.md`
at release time and deletes them in the same operation. This README is a tracked
placeholder that keeps the directory present between releases; it is never
treated as a fragment.
