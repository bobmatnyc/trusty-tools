Fixed
- The cargo ownership guard is now infallible-by-construction when
  interrupted: a guard dropped without settling (the daemon's detached
  `tokio::spawn` upgrade tasks dying on shutdown, a panic unwind, or a
  Ctrl-C abort mid-`cargo install`) restores its moved-aside binaries from
  `Drop`, and `move_aside` sweeps stale `.{name}.pre-cargo.*` asides whose
  owning pid is dead — restoring the aside when the destination is missing
  (SIGKILL, where `Drop` never runs) and deleting it otherwise. Previously
  an interrupted upgrade could leave the destination empty with the only
  copy stranded under a hidden aside name, breaking the next launchd
  respawn (#5777, code-critic round on PR #5778).
