Added

- `search_rpc`, the one trusty-search socket client, moved down from `trusty_mpm::daemon::search_rpc` so the two consumers share one implementation (#7237). It exposes `search_socket`, `call_at`, the synchronous `call_blocking` (its own OS thread and runtime, for callers inside a tokio runtime), the daemon's method-name constants, and `SearchRpcError` carrying the daemon's own JSON-RPC code. Behind the `uds` feature, which `search-index` now implies.
