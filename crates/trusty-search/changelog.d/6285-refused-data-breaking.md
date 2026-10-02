Breaking
- `service::daemon_client::DaemonCallError::Refused` gains a `data: Option<serde_json::Value>` field. Code that constructs the variant, or matches it without `..`, must add the field (#6285).
