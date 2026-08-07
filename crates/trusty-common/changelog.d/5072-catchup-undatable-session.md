Fixed

- Catch-up no longer returns only the one paused session it cannot date ([#5072](https://github.com/bobmatnyc/trusty-tools/issues/5072))
  - `session_finder::parse_trusty_mpm_session` derived `paused_at` from the `session-YYYYMMDD-HHMMSS.md` filename alone, so a hand-written snapshot such as `session-20260730-bounce.md` had no timestamp. The watermark filter — `s.sort_key().is_none_or(|ts| ts > wm)`, duplicated verbatim in `catchup/mod.rs` and `catchup/json.rs` — reads an unknown key as "newer than the watermark", so that one undatable record was admitted by every watermark while all 99 well-formed snapshots in the same directory were correctly dropped
  - an undated filename now falls back to the file's mtime, and the duplicated predicate is one `session_finder::filter_sessions_since` that fails closed: a session with no derivable pause instant is excluded from a watermark-filtered digest and the drop is logged, never silently admitted. `full=true` still returns everything
