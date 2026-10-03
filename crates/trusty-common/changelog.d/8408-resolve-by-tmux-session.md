Added
- `catchup::resolve::resolve_snapshot_for_caller` resolves a relaunched
  session's own snapshot by its tmux session name when the window route
  misses, and reports it as `ResolutionPath::TmuxSession`
  (`"tmux_session"`). A relaunch recreates the tmux window, so the window id
  the caller holds afterwards never matched its last pause. A digits-only
  session name never matches. `catchup::resolve::session_name_of` is the new
  parser, and the digest's ownership test accepts the same match.
