Added

- `tm launch --twin` arms the `claude` it starts for supervisor-twin mode
  (#8878). The session counts as a twin only when all of these hold: the
  project is in `[supervisor.twin] projects` and `[supervisor] projects` in
  `~/.trusty-mpm/config.toml`, the session runs the supervisor profile,
  `tm launch --twin` recorded that `claude` process by PID and start time, and
  the call comes from the main thread, not a subagent. Any read error, parse
  error or mismatch means not a twin. `TRUSTY_MPM_PM_UNRESTRICTED` and
  `TRUSTY_MPM_DISABLE_HOOKS` never make a session a twin. This release only
  establishes the identity: no `tm hook --pm-guard` decision reads it yet.
- `tm launch --twin` refuses `--worktree`, and checks the twin grant for the
  managed checkout before it clones or provisions anything, so a refused launch
  leaves nothing behind (#8878). The armed `claude` must be the hook's direct
  parent, so a session started from the twin's own shell is never a twin.
