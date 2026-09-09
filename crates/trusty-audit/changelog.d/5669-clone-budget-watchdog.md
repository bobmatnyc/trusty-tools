Fixed

- `--budget-gb` is now a cap on what the clones occupy, not only a gate on what
  they start. It used to be checked between repositories, so 19 GiB spent
  against a 20 GiB ceiling still admitted a 100 GB monorepo and finished at
  119 GiB — on a client's own machine, during an unattended engagement, with
  nobody watching to intervene. While a clone runs, its staged tree is now
  measured every 500ms and the clone is signalled once `spent + staged` crosses
  the ceiling: `SIGTERM` first, so `git` and `gh` remove their own temporary
  pack files, then `SIGKILL` after a five-second grace, and the child is reaped
  either way. The signal goes to the clone's whole process group, because a
  clone is a tree of processes — `gh` forks `git`, `git` forks `ssh`,
  `git-index-pack` and `git-unpack-objects`, and those grandchildren are what
  hold the sockets and write the bytes. Signalling only the process this crate
  spawned left them fetching into, and recreating, the staged directory the
  cleanup had just removed. The partial tree is removed by the same path a
  failed clone takes, so nothing survives under staging or is promoted into
  `repos/` — and when that removal itself fails, the surviving bytes are
  measured, counted against the budget, and named in a gap line of their own
  rather than silently ignored. The overshoot is bounded by the sampling
  interval rather than being zero: a clone can exceed the ceiling by what it
  writes in one sample period.
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
- Every disk figure now comes from one measuring function that tells those two
  cases apart, and all three of its call sites — the watchdog's sample, the
  freshly promoted checkout, and a checkout reused from an earlier run — get the
  same answer. The last two used to collapse an unopenable root to zero bytes
  and feed that straight into the budget ledger, so a reused checkout of any
  size could be spent invisibly. Both are now named gaps instead. A checkout
  that was already on disk when the run started is never removed.
- Ctrl-C during a clone now stops the clone. Putting each clone in a process
  group of its own is what lets one signal reach the whole tree, and it is also
  what takes that tree out of the terminal's foreground group — so a raw Ctrl-C
  used to kill `taudit` outright, running no destructors, and leave `ssh` and
  `git-index-pack` fetching into the client's disk with the watchdog dead. The
  interrupt is now forwarded to every clone group the terminal can no longer
  reach, anything still running 250ms later is killed, and `trusty-audit` then
  dies by `SIGINT` under its default disposition, so a shell script or CI runner
  still sees the run as interrupted rather than as finished.
- A partial checkout that can be neither removed nor measured is now reported as
  an unknown size naming the measurement failure, instead of as "at least 0
  bytes are still on disk" — a figure that reads as measured for a tree that may
  hold gigabytes. Its bytes are excluded from the budget ledger and every later
  budget decision in the run is marked a floor.
- `CloneOptions::budget_bytes`, `DEFAULT_BUDGET_BYTES`, and the `--budget-gb`
  help text no longer say the budget stops clones from starting without capping
  one in flight.
