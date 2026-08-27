Fixed

- Decommissioning a tm-owned worktree through the MCP `session_decommission`
  tool or the idle reaper no longer leaves a stale entry in `git worktree list`
  (refs [#5949](https://github.com/bobmatnyc/trusty-tools/issues/5949)). Both
  call `SessionManager::decommission` in-process, below the client layer that
  carried the repair for the routed HTTP paths, so the owned-workspace branch
  removed the directory with `remove_dir_all` and git kept listing it — the
  reaper unattended. The repair now runs inside `decommission_with_root`, which
  every caller reaches regardless of transport, and it targets the checkout git
  itself reports as owning the worktree rather than the parent directory.
