Added
- `tm session disk [<id-or-name>] [--json]` reports disk usage per session. With
  no argument it lists one row per session of the current project, bytes
  descending, with a total; with a session id or friendly name it breaks that
  session down into build-directory bytes against the rest and lists every
  worktree it owns across every project. It runs no survey of its own — one
  `disk_survey` call with `group_by: "session"` over the daemon's `POST /rpc`,
  so the walk, the classification and the by-session fold stay the daemon's. An
  id no worktree is attributed to exits non-zero rather than reporting zero
  bytes. (#7313)
