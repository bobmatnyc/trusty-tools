Security

- allowlisted untracked files (default `.env*`) are no longer copied into a session worktree until git confirms they will be ignored there (closes [#4733](https://github.com/bobmatnyc/trusty-tools/issues/4733))
  - the `info/exclude` registration ran AFTER the copy and a failure was only a `warn!`, so a worktree whose `git rev-parse` merely failed — a stale gitlink, `detected dubious ownership`, an unreadable `.git` — was left holding the operator's `.env` unregistered, where a later `git add -A && git commit` stages it into history
  - the order is inverted: register first, then re-verify each path with `git check-ignore` (git's own authority, mirroring `native_mcp::is_env_local_actually_ignored`) and copy only what it confirms
  - this also stops a secret overwriting a path the repo already TRACKS — git reports tracked paths as not-ignored no matter what `info/exclude` says, and `git add -A` would stage the change
  - a destination with a corroborated absence of any repository has no history to leak into and is still copied to freely, so non-git destinations are unaffected
