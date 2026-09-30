Added
- A 503 index-unavailable refusal on the daemon's Unix socket now carries its whole HTTP body (`index_id`, `retryable`, `restore_via`, `reason`, `transient`, `stages`) as the JSON-RPC error's `data`, so those fields survive the move off HTTP. Other refusals are unchanged (#6285).
- `service::daemon_client::DaemonCallError::data()` returns that detail from a refusal (#6285).
