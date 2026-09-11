Added

- The daemon now reclaims a merged pull request's worktree and local branch on
  its own, with no `tm session prune-worktrees --merged-prs` for the PM to
  remember (#7504). The trigger is `gh`-reported merge state, re-read per
  candidate immediately before each deletion, never a timer's guess.
- A worktree a live process was launched from — the daemon's own working
  directory, or the executable serving the sweep — is spared and reported
  (#7504).
- Each reclaim writes an audit line carrying path, branch, pull request and bytes
  freed; a removal that does not complete is logged as a failure rather than
  folded into the success line (#7504).
- `TRUSTY_MPM_WORKTREE_RECLAIM=0` disables the sweep and
  `TRUSTY_MPM_WORKTREE_RECLAIM_INTERVAL_SECS` overrides its one-hour cadence
  (#7504).
