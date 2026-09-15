Fixed

- `tm issue seed-labels` now takes a repeatable `--only <name-or-family>` filter,
  so a caller needing one lifecycle family no longer creates the whole
  `issue-state.yaml` vocabulary plus the policy set repo-wide (18 labels in the
  reported run). A value ending in `:` or `/` selects that family; any other
  value is an exact label name; a value matching nothing is an error before any
  `gh` call, not a zero-label success. An invocation without `--only` is
  unchanged ([#7983](https://github.com/bobmatnyc/trusty-tools/issues/7983)).
- `issue-state.yaml` gained the `status:merged -> closed` edge, so the docs-only
  close path can run through `tm issue transition` instead of falling back to a
  bare `gh issue close` that leaves no audit comment and no label removal
  ([#7647](https://github.com/bobmatnyc/trusty-tools/issues/7647)).
- The same gap blocked CLAUDE.md's rung 1-3 close-at-merge rule and every
  administrative fold, so `status:coded -> closed`, `status:in-progress ->
  closed` and `open -> closed` landed alongside it. All four carry
  `requires_note: true`: closing early changes what the evidence is, never
  whether evidence is required
  ([#7647](https://github.com/bobmatnyc/trusty-tools/issues/7647)).
