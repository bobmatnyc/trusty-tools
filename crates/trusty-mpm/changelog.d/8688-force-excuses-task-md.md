Fixed

- `tm session decommission --force` now removes a task-bearing managed
  worktree that holds only tm-written files. The timestamped
  `.claude/settings.json.<timestamp>.bak` snapshots are now excused alongside
  the other provisioning files. The untracked `TASK.md` is excused only while
  its bytes equal the task tm wrote there at spawn; an edited `TASK.md`, or
  one with no session task to compare against, keeps the worktree and is
  named in the refusal. User work and unpushed commits still keep it too.
- `--force` now also removes the worktree of a session that errored once.
  A failed spawn appends one ` [error: …]` note to the session task after
  `TASK.md` was written, so the unedited `TASK.md` is matched against the task
  text before that note. This applies only when the task holds exactly one
  ` [error: ` marker and ends with `]`. A session that errored more than once,
  or whose task text itself contains the marker, keeps its tree.
- A decommission refusal now states a file count that matches the entries it
  lists: the count and the list now use the same excuse set.
