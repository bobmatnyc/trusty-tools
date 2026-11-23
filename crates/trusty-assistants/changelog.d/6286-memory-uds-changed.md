Changed
- Every trusty-memory read and write goes through the one shared client, `trusty_common::memory_rpc`, rather than three independent `reqwest` clients against `/api/v1/*` — the routes those targeted no longer exist. "No such palace" stays a clean empty result rather than an error: it reads `MemoryRpcError::is_not_found` where it used to read a 404 status
