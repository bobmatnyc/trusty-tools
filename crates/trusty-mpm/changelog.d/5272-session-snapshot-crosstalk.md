Fixed

- `session_context_catchup` no longer hands a session another session's pause snapshot ([#5272](https://github.com/bobmatnyc/trusty-tools/issues/5272))
  - resolution used to fall through to "newest pause overall", then `LATEST-SESSION.txt`, then an mtime scan — all three session-blind. A session with no snapshot of its own got whichever one existed, with nothing in the response saying whose it was. Correct under the one-session-per-checkout model that introduced the chain ([#2731](https://github.com/bobmatnyc/trusty-tools/issues/2731)); wrong now that the PM runs on the project's main checkout and several sessions share one `.trusty-mpm/sessions/` store
  - `resolved_snapshot` is now resolved strictly for the `session_id` you pass. No id, or an id that owns no snapshot, returns null
  - reading another session's state still works — pass that session's id. That explicit request is the only way a cross-session read happens
  - `session_context_pause` writes snapshots to `.trusty-mpm/sessions/<session-id>/`. Existing flat `session-YYYYMMDD-HHMMSS.md` files at the store root are untouched and still resolve, through the `sessions-log.jsonl` line that attributes them; a flat file with no log line resolves for nobody rather than for whoever asks
  - the catch-up digest scans per-session directories as well as the store root, so snapshots written under the new layout still appear in `sessions[]`
  - a `sessions-log.jsonl` entry naming a path outside the store is refused instead of read
