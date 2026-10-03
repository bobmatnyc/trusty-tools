Added
- `catchup::resolve` resolves a relaunched session's own snapshot by its tmux
  session name when the window route misses, and reports it as
  `ResolutionPath::TmuxSession` (`"tmux_session"`). A relaunch recreates the
  tmux window, so the window id the caller holds afterwards never matched its
  last pause. The route needs the caller's tmux `#{session_created}`
  (`CallerIdentity::with_tmux_session_created`, used by the new
  `resolve_snapshot_for_identity`) and matches only snapshots paused after it,
  so a later session reusing the name never claims an earlier session's
  snapshot. `resolve_snapshot_for_caller` carries no creation time and never
  takes this route. A digits-only session name never matches.
  `catchup::resolve::session_name_of` is the new parser.
- `redact_sessions_not_owned_by` grants ownership by tmux session name only
  to the one snapshot the resolver answered, never to every entry sharing the
  name.
