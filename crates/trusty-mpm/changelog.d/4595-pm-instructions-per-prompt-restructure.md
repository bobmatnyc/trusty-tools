Changed

- the composed PM prompt drops 18,947 bytes (52,921 → 33,974, −35.8%; ~20.4k → ~13.1k tokens at 2.6 chars/token) by applying the owner's per-prompt rule — an instruction not needed on EVERY prompt lives in a skill and keeps a one-line trigger here ([#4595](https://github.com/bobmatnyc/trusty-tools/issues/4595))
  - moved to `tm-delegation-patterns`: the retry protocol, task-complexity sizing, the batching anti-pattern table, per-agent model overrides and the cost model, the `code-critic` dispatch standard, `isolation: "worktree"`, cross-workstream claim drawers, the structural delegation brief, and the architecture-suggestion cap
  - moved to `tm-workflow`: the per-phase dispatch-brief templates, the override commands, and the close-and-fold / branch-vs-worktree half of the sprint-then-harden doctrine
  - moved to `tm-pr-workflow`: the pre-push credential scan, which the PM delegates rather than runs
  - moved to `tm-circuit-breaker`: the Quick Violation Detection list, which restated the two tables above it
  - reduced to pointers, with the imperative left resident: the customization mechanics, the Trusty tool-priority per-tool tables, the direct-action budget's `pm_guard` detail, and the `Fail-Open Check`'s five steps
  - the framework floor keeps every imperative — the Prohibitions table, the Circuit Breakers table, the direct-action budget with both its halves, and the Framework-Guaranteed Conventions
- the PM asks the user based on observable conditions (ambiguous requirements, a missing credential, an irreversible architecture choice, an unrequested destructive step) rather than an uncalibrated "<90% success probability" estimate
- the four-part report template applies to task-completion reports only, not to every PM response
