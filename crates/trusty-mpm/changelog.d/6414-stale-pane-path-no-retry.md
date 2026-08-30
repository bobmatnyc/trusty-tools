Fixed

- Boot reconciliation no longer sleeps through a retry that cannot change its
  answer. A live tmux pane whose recorded working directory has been deleted — a
  reaped agent worktree — was probed three times with the full 200 ms backoff
  before being declined, even though tmux keeps reporting the same recorded path
  on every probe. On a host with 276 such panes that was 55 s of sleep inside
  `reconcile_on_boot`, which `DaemonState::session_manager()` awaits before it
  hands the manager to its first caller; the SessionEnd worktree sweep is often
  that caller and reached its first gate a minute late. The retry still runs in
  full when tmux returns no answer at all, which is the transient failure it was
  added for (#6118), and no pane's verdict changes — measured on 294 live
  sessions, 60.65 s to 2.12 s (#6414).
