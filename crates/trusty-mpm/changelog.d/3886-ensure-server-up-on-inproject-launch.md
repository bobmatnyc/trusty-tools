Fixed

- `tm` launch-new-session no longer fails on a host where tmux has never run (closes [#3886](https://github.com/bobmatnyc/trusty-tools/issues/3886))
  - the #3823 server-up guard moved into `SessionManager::resolve_session_name`, adjacent to the `list-sessions` probe it protects, so the in-project launch routes (`spawn_managed_on_main`, `reserve_inproject_worktree`) that bypass `create_with_id` inherit it
  - `tmux error:` is no longer printed twice — `names_for_serial_allocation` and `dedupe_session_name` propagate the already-typed `ManagedError` instead of re-wrapping it
