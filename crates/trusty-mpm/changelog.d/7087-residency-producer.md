Added

- trusty-mpm now serves the active-project set over its Unix socket as
  `mpm.residency.active` (#7087 slice 1b): every managed session in state
  `Active`/`Provisioning` whose tmux pane a live `tmux list-sessions` still
  confirms, grouped by project root into a
  `trusty_common::residency::ActiveProjectSet`. A `tmux` probe failure fails
  closed — every persisted `Active`/`Provisioning` record is served rather
  than the set collapsing to empty.
