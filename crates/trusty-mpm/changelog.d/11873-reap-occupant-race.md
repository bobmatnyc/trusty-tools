Fixed
- The reap test `session_end_reaps_a_finished_agents_released_worktree` no longer keeps its tree about one run in ten: the shared git fixture sets `maintenance.auto=false` in its repo, so its `git commit` and `git fetch` no longer leave a detached `git maintenance run --auto` standing in the worktree for the occupant probe to see when the global git config does not disable it (Refs #11873)
