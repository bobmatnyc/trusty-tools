Removed
- `run_http`, `run_http_dynamic`, `run_http_on`, `bind_dynamic_port`, `http_addr_path`, and the `web` and `foreground` modules. `transport::uds::serve` and `transport::uds::socket_path` replace them
- The `/api/v1/recall`, `/api/v1/kg/prompt-context`, `/api/v1/kg/aliases`, `/api/v1/kg/prompt-facts`, `/api/v1/kg/gaps`, chat-session CRUD and `POST /api/v1/activity/hook` routes retired without a folded equivalent: every one duplicated a method `transport::rpc::dispatch` already served
- `DEFAULT_HTTP_PORT`. It survived pass A only as a compile-time stub for `trusty-agents`' REST client; that client dials the socket now, so the constant goes with it and nothing in the workspace names 7070
