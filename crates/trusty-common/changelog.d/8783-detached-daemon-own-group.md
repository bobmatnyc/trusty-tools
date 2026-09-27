Fixed
- A daemon started through `daemon_guard::spawn_detached` or `spawn_current_exe` (and their `_forwarding_parent_link` forms) now starts in its own session (`setsid`, Unix), so a SIGKILL or Ctrl-C aimed at the spawning CLI's process group no longer kills it. New `daemon_guard::start_in_new_session` gives a daemon start with its own stdio or cwd the same detachment (#8783).
