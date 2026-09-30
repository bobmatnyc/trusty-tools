Added
- `uds::server::RpcError` now carries JSON-RPC 2.0's optional `data` member, set with the new `RpcError::with_data` builder. It is omitted from the frame when unset, and a frame from an older peer with no `data` still parses (#6285).
- `search_rpc::SearchRpcError` carries the daemon's `data` member, so `search_rpc::call_at` callers read a refusal's structured detail, not only its code and message (#6285).
