Fixed

- `tm session decommission --force` now removes a task-bearing managed
  worktree that holds only tm-written files. The untracked `TASK.md` and the
  timestamped `.claude/settings.json.<timestamp>.bak` snapshots are now
  excused alongside the other provisioning files; user work and unpushed
  commits still keep the worktree.
- A decommission refusal now states a file count that matches the entries it
  lists: both come from one per-file status read and one excuse set.
