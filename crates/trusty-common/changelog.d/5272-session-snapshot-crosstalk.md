Fixed

- `catchup::session_finder::latest_trusty_mpm_snapshot` no longer resolves across session boundaries ([#5272](https://github.com/bobmatnyc/trusty-tools/issues/5272))
  - it now requires a session id and resolves only snapshots that `sessions-log.jsonl` attributes to it, via the new `session_log::resolve_session_snapshot`. Passing `None` returns `None` instead of the newest snapshot overall
  - `session_log::resolve_latest_snapshot` keeps the session-blind fallback chain but is now used only by the legacy claude-mpm JSON store
  - `catchup::pause::write_pause_snapshot` writes under `sessions/<session-id>/` and records the snapshot path relative to the store root, so one containment-checked join serves both the new and the pre-#5272 flat layout
  - `find_paused_sessions` scans per-session directories in addition to the store root
  - a log entry whose `snapshot` escapes the store (absolute path or `..`) is refused rather than read
