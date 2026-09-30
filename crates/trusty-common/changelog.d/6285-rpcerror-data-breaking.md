Breaking
- `uds::server::RpcError` and `search_rpc::SearchRpcError` each gain a public `data: Option<serde_json::Value>` field. Code that builds either with a struct literal, or destructures one without `..`, must add the field; `RpcError::new` and the other constructors are unchanged (#6285).
