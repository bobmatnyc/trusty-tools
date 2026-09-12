Fixed

- The zero-fold decline no longer claims the `💸` statusline segment disappears.
  `warn_no_fold_once` and the `tm doctor` instruction-compression check both said
  the segment "stays absent" for a project that overrides no instruction section,
  which stopped being true at #7617: the statusline folds `divert` and `compress`
  rows beside instruction-compression, falls back to a linked sibling session,
  and renders `💸—` as an explicit empty state. Both messages now scope the claim
  to the one technique — this project contributes nothing under it until a
  CLAUDE.md section override folds a bundled section away, while divert and
  compress savings still count and the segment still renders
  ([#7671](https://github.com/bobmatnyc/trusty-tools/issues/7671)).
