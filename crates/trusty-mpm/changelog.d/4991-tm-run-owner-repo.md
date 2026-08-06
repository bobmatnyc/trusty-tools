Added

- `tm run <owner>/<repo>` cold-starts a daemon-managed session for a repo that is not on disk yet
  - clones to the managed checkout at `~/trusty-mpm-projects/<owner>/<repo>`, then hands off to the existing `tm launch` path — so the result is a real `SessionRecord` with a tmux pane, visible in `tm ls` and `tm sessions`, not a blocking foreground `claude`
  - the positional is classified by the same predicate `tm register` uses, so `tm run bobmatnyc/trusty-tools` and `tm register bobmatnyc/trusty-tools` cannot disagree about what a string means; a registered alias keeps the unchanged DOC-24 standalone behaviour
  - reusing an existing managed checkout FAILS LOUD when its `origin` names a different repository — no auto-repoint, and the refusal lands before anything writes to the directory
  - a dirty existing checkout is not an error: the fast-forward is skipped and said so in normal output, while the fetch still runs. The session's branch is cut from a freshly-fetched `origin/<default-branch>`, so it never inherits the checkout's local `HEAD`
  - `tm run ./some/dir` now reports that a relative path is not a repository instead of "alias not found"
