Changed

- `version-control` agent — the PR Workflow now opens every PR through
  `tm pr open --title <t> --body-file <path> [--issue N] [--rung 1-6] [--base
  main] [--docs-only]` instead of hand-assembling `gh pr create`. It names the
  seven-field body contract, the exact attribution footer, the shipped
  `--assignee @me --label trusty-mpm --label ws/<session>` defaults, the
  `scripts/check_changelog_fragment.sh` gate that runs before `gh` is ever
  spawned, the `--issue N` / `Refs #N` (never `Closes` without `--closes`)
  rule, and `--dry-run` for previewing the argv — with hand-assembled
  `gh pr create` kept only as the fallback where `tm` is absent. The
  Deterministic Tools table gained rows for `tm pr open`, `tm pr
  queue-check`, `scripts/required-checks.sh`, and `scripts/is-branch-caused.sh`
  (Refs #6659).
