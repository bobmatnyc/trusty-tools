Fixed

- The session reaper now removes a `SessionStart`-announced session whose
  bound `claude` has exited, and stales its live delegations. Before, such a
  session stayed Active for the daemon's lifetime. A `claude` counts as gone
  only on proof: no process holds its pid, or the process holding it started
  at another time (a reused pid). A session whose `claude` still runs is
  kept, and so is one whose process lookup cannot answer.
