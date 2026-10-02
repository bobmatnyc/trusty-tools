Fixed

- The session reaper now removes a `SessionStart`-announced session whose
  bound `claude` has exited, and stales its live delegations. Before, such a
  session stayed Active for the daemon's lifetime. A `claude` counts as gone
  only on proof: no process holds its pid, or the process now holding it
  started at another time and is not a `claude`. Just before it removes the
  session, the reaper checks again, and keeps the session when the id was
  rebound since the probe, the daemon is resuming it, a hook event for it
  arrived within the last reap interval (60 s), or a later `claude` that
  announced the id over the daemon socket still runs. A session whose
  process lookup cannot answer is also kept. A later announcer is held in
  memory only, so after a daemon restart an idle `claude --resume <id>` run
  by hand is kept only by its hook events.
