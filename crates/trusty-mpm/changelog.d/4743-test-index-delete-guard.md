Fixed

- `cargo test` can no longer destroy a real trusty-search index (closes [#4743](https://github.com/bobmatnyc/trusty-tools/issues/4743))
  - `session_manager::search_gc` formatted `DELETE /indexes/{id}?delete_data=true` at two sites and sent it to whatever daemon `resolve_daemon_base_url` discovered — under a test run, the operator's live one on port 7878. `?delete_data=true` destroys the index's on-disk data directory
  - fixture workspaces derive their index id from a bare `file_name()`, so `decommission_full_still_terminates_the_runtime` asked that daemon to destroy an index named `full`; `sess`, `live` and `proj` appear the same way elsewhere in the suite
  - both sites now go through one `DestructiveIndexDelete` capability that holds the only copy of the `?delete_data=true` literal and exposes no constructor taking a base URL, so a caller cannot build the request without acquiring it — and a `cargo test` process never can
  - the orphan sweep acquires the capability before listing anything, so a test process makes no request at all instead of enumerating real indexes and stopping at the delete
  - `TRUSTY_ALLOW_PRODUCTION_STATE=1` remains the explicit opt-in for a test that deliberately drives a real daemon
