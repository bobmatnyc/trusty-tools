Fixed
- A daemon started through `daemon_guard::spawn_detached` or `spawn_current_exe` (and their `_forwarding_parent_link` forms) now starts in its own session (`setsid`, Unix), so a SIGKILL or Ctrl-C aimed at the spawning CLI's process group no longer kills it (#8783).
- A detached `uds::UdsServiceSupervisor` child (`SupervisorConfig::with_detached`, used for the on-demand trusty-analyze daemon) now starts in its own session too, so it outlives a group kill or Ctrl-C aimed at its caller. A supervised child stays in the caller's group (#8783).
