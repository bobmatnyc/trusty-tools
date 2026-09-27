Added
- `daemon_guard::start_in_new_session(&mut Command)` makes a spawned process lead a new session (`setsid` in a `pre_exec` hook, Unix; a no-op elsewhere), for a daemon start that needs its own stdio or cwd and so cannot use `spawn_detached`. Combined with `Command::process_group` the spawn fails with `EPERM`, because a group leader cannot call `setsid` (#8783).
