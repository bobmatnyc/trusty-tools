Fixed

- session worktrees are now branched from a freshly-fetched `origin/<default-branch>` instead of the base checkout's local `HEAD`, which left every new session as stale as the operator's last `git pull` (closes [#4957](https://github.com/bobmatnyc/trusty-tools/issues/4957))
  - the default branch is resolved from the repo, never hardcoded to `main`
  - a failed fetch falls back to the last-known remote-tracking ref (or `HEAD`) and logs a warning naming the stale tip, rather than reporting a clean success
  - a repo with no remote still branches from `HEAD`, with no spurious warning
