Added

- **A relevance floor for recall results ([#5037](https://github.com/bobmatnyc/trusty-tools/issues/5037)).** `DEFAULT_RELEVANCE_FLOOR`,
  `FloorOutcome`, and `apply_relevance_floor` in a new
  `memory_core::retrieval::relevance` module. Every retrieval path in `layers.rs`
  ended in `truncate(top_k)` and nothing else, so a query with no good answer
  still returned a full `top_k` of whatever ranked highest — including L1
  drawers scoring `importance * L1_NO_SIMILARITY_PENALTY`, at most `0.15`.
  `apply_relevance_floor` is the one implementation of "below the floor, it is
  not shown", and it returns the count of what it dropped so a caller can say so
  rather than going silent. An item whose score is unknown is kept, never
  dropped.

  The default is `0.35`, picked from measured distributions against the live
  1,332-drawer `trusty-tools` palace rather than guessed: 150 candidates from 15
  off-topic prompts span 0.1500–0.3439 (75 of them at exactly 0.1500, the L1
  penalty), while 57 self-retrieval correct-drawer hits span 0.4844–0.9743 and
  1,200 candidates from real logged hook prompts span 0.4042–0.7527. `0.35` is
  the smallest swept value at which no off-topic candidate survives.

  `recall`/`recall_scoped` are deliberately unchanged: `truncate(top_k)` stays a
  length cap, and gating inside it would change every MCP and CLI recall
  caller's contract. Callers that must not show a weak match apply the floor to
  what comes back.
