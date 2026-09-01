Added

- Per-index embedding pause/resume over the socket: `search.index.pause_embedding` and `search.index.resume_embedding` take `{"index_id"}` and answer `{"index_id", "embedding_paused"}`. Both are idempotent and refuse an unknown index with `404`. A paused index stops embedding at its next batch boundary and keeps its pending work; BM25, KG and the file watcher are unaffected. The state is in-memory and does not survive a daemon restart (#6524).
- `search.index.status`'s `stages.semantic` carries `paused: bool` beside the existing `status`, so a consumer can tell a parked stage from a running one without a new status variant (#6524).
- Per-index file-change feed: `search.index.file_events` replays the last 200 watcher-observed changes — `{path, kind: "modified"|"removed"|"rescan", at_unix_ms}`, path relative to the index root — then streams live ones. In-memory only (#6524).
