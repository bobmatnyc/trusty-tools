Fixed

- `--budget-gb` is now a cap on what the clones occupy, not only a gate on what
  they start. It used to be checked between repositories, so 19 GiB spent
  against a 20 GiB ceiling still admitted a 100 GB monorepo and finished at
  119 GiB — on a client's own machine, during an unattended engagement, with
  nobody watching to intervene. While a clone runs, its staged tree is now
  measured every 500ms and the child is signalled once `spent + staged` crosses
  the ceiling: `SIGTERM` first, so `git` and `gh` remove their own temporary
  pack files, then `SIGKILL` after a five-second grace, and the child is reaped
  either way. The partial tree is removed by the same path a failed clone takes,
  so nothing survives under staging or is promoted into `repos/`. The overshoot
  is bounded by the sampling interval rather than being zero — a clone can
  exceed the ceiling by what it writes in one sample period.
- The outcome lands on its own `CloneState::BudgetExceeded` rather than on
  `Failed` or on the existing start-gate `Skipped`, and renders as
  `OVER BUDGET — stopped mid-clone at <size> against a <size> ceiling`. A
  recipient reading `FAILED` would go looking for a wrong repository name or a
  revoked credential; there is no fault here, and the gap line says to raise
  `--budget-gb` or drop the repository instead.
- A sample the watchdog cannot take now fails the clone CLOSED — the child is
  killed and the attempt reported — rather than letting the clone run on
  unmeasured. That includes a staged tree whose own root cannot be opened, which
  previously counted as zero bytes and so could never trip the ceiling. A
  partial walk below the root stays a floor and is not a failure, because `git`
  creating and removing pack files mid-fetch makes one ordinary.
- `CloneOptions::budget_bytes`, `DEFAULT_BUDGET_BYTES`, and the `--budget-gb`
  help text no longer say the budget stops clones from starting without capping
  one in flight.
