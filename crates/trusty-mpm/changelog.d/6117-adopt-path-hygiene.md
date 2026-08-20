Fixed

- Boot reconciliation adopts a live tmux pane under an id derived from the
  pane's tmux name, so one pane can only ever occupy one store row
  ([#6117](https://github.com/bobmatnyc/trusty-tools/issues/6117)). The
  external-adopt loop decides from a snapshot of store names taken once, before
  the loop; a second adopter holding its own copy saw the same pane as unknown
  and wrote a second record under a fresh random id. The store is keyed by id,
  so both survived — 11 tmux names carried two records apiece in the reporting
  store, several pairs written 30-60 ms apart. A name-derived id makes the
  second write land on the first's key instead of beside it.
- Boot reconciliation no longer adopts a pane whose working directory it cannot
  resolve ([#6118](https://github.com/bobmatnyc/trusty-tools/issues/6118)). Such
  a pane used to become an `Active` record with a `/unknown` cwd and an
  `unmanaged` note in `task`, and that record could never be retired: the
  `tm ls` auto-prune keeps any record whose tmux name is live, and the orphan-GC
  keeps any pane a registry names — so adopting the pane is precisely what made
  it permanent. 55 of the 103 records in the reporting store were these. The
  pane is now left untracked and named in the daemon's boot log, which hands it
  to the orphan-GC: it kills a managed-prefix pane only when the pane is an idle
  shell with no live child process, seen idle on two consecutive sweeps, and it
  keeps one still running an agent. Adoption of a pane whose cwd does resolve is
  unchanged.
- The working-directory probe behind that decision retries before giving up. It
  is a kill decision now — a declined pane is orphan-GC input — and
  `TmuxDriver::pane_current_path` reports a failed spawn, a non-zero exit and
  empty output all as `None` with no retry, while boot reconciliation runs once
  per daemon process. One flaky answer would have cost a live pane inside two
  minutes.
- `ReconcileReport` carries `adoption_declined`, the tmux names reconciliation
  declined this pass, and the daemon's boot summary logs the count and fires on
  it, so a pane that stops appearing in `tm ls` is accounted for rather than
  silently absent.
