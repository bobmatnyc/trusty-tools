Fixed

- `git-workflow` skill — gained a "trusty-tools Deterministic Tools" section
  naming the exact commands (`check_changelog_fragment.sh`,
  `check-pr-version-bump.sh`, the live required-contexts read, the
  merge-queue-ownership query, the one-shot pre-merge status read, and
  `tm session prune-worktrees`) `version-control` runs itself before opening
  or merging a PR on this repo, replacing generic-only prose.
- `tm-delegation-patterns` skill — the `version-control` row's `tag` verb is
  now qualified: a plain annotated tag on explicit PM instruction is
  `version-control`'s; a release tag bound to a `cargo publish` is
  `local-ops`'s.
- `tm-workflow` skill — the Changelog Requirement section now names
  `scripts/check_changelog_fragment.sh` as the self-check to run before
  pushing.
