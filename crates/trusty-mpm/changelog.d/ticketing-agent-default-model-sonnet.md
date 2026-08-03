Changed

- `ticketing` agent's default model tier is now `sonnet`, up from `haiku`.
  Duplicate-detection and scope-boundary judgement (is this issue already
  filed, is this work in scope for a milestone) are judgement calls, not
  clerical ones — observed 2026-08-03: a haiku-tier ticketing agent filed a
  duplicate issue after being told not to, and cleared milestones on a shipped
  release when asked only to report them.
