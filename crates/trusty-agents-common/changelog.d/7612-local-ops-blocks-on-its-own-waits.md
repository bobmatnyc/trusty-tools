Fixed

- `local-ops.md`'s "Long Waits" section now names the blocking verb for the two
  task shapes the agent kept parking on. A deploy script and a reachability poll
  are the agent's own commands, so the section states they run in the
  foreground and that the wait goes through `tm wait --for run --pid` (or
  `--for file --path` for a probe that writes a sentinel), never through a
  notification the agent expects to wake it. The two narrations observed —
  "I'll wait for the deploy monitor notification before proceeding" and
  "Standing by" — are quoted as the shapes that end a turn with the goal unmet
  (#7612).
- The same section now states a margin rule for any bounded polling loop: bound
  it ~10-15% under the harness's foreground ceiling rather than at it. The Bash
  tool caps at 600000ms and a loop written to a nominal 600s overran that on
  per-iteration overhead and auto-backgrounded; ~480s is the budget, re-issued
  in the same turn when the condition has not met yet. #2501 and #2610 closed
  this failure class on version-control and merge-release; this is the
  recurrence on local-ops, whose task shapes the earlier fix did not reach
  (#7612).
