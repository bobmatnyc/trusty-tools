Added
- `catchup::json::snapshot_json` maps one native session snapshot file to its `PausedSessionJson` record with the parser the digest uses, so the lean `session_context_catchup` default can return the resolved snapshot without scanning the store (#9883).
