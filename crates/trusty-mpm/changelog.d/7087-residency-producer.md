Added

- trusty-mpm now serves the active-project set over its Unix socket as
  `mpm.residency.active` (#7087 slice 1b): every managed session in state
  `Active`/`Provisioning` whose tmux pane a live `tmux list-sessions` still
  confirms, grouped by project root into a
  `trusty_common::residency::ActiveProjectSet`. A `tmux` probe failure fails
  closed — every persisted `Active`/`Provisioning` record is served rather
  than the set collapsing to empty.
- The residency generation now moves on `resume`, `mark_reactivated`,
  `mark_errored`, the runtime-exit reaper's `Stopped` transition, a forced
  record delete, and the boot reconcile's leaked-test-adoption sweep, joining
  the create/stop/adopt/decommission bumps already there — so a consumer
  polling `mpm.residency.active` can no longer be handed a stale generation
  across any of them (#7087). A session reaching a terminal state also drops
  its cached palace/index derivation, bounding that cache by live records
  rather than by every session the daemon has served.
