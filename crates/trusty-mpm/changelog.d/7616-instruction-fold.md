Fixed

- Instruction compression now folds real bytes for a project that overrides no
  `CLAUDE.md` section. The composer gained a compose-time fold that strips
  authoring-only content — HTML comments, trailing whitespace, repeated blank
  lines, Markdown table padding — from the delivered PM prompt without touching a
  rule, a prohibition row, a circuit-breaker row or a `Skill()` pointer, and the
  savings producer now counts the undeduped agent roster (#4513) as source it
  read and partly discarded. Before this, `compiled >= sources` was structurally
  permanent for such a project and no ledger row was written. (#7616)
- `tm doctor` gained an `instruction_compression` check that states the measured
  fold, or names it INACTIVE with both byte counts, instead of leaving a missing
  💸 statusline segment as the only evidence. (#7616)
