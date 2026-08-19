Fixed
- Loopback daemon calls no longer go through an exported `HTTP_PROXY` /
  `http_proxy` / `ALL_PROXY`. reqwest 0.12 routes `127.0.0.1` through the proxy
  — hyper-util's matcher has no loopback exemption — so on a machine with a
  proxy configured every daemon call failed and the caller reported a healthy
  daemon as down. The new `http_client` module is the one entry point that
  applies `.no_proxy()`: `loopback_client_builder` (proxies off, caller keeps
  its own timeouts), `loopback_client` (plus 2s connect / 5s request bounds),
  and `blocking_loopback_client_builder` behind the new `blocking-http` feature.
  `daemon_http_client`, `health_probe`, `daemon_guard`, `mcp::daemon_bridge`,
  `mcp::memory_rpc`, `search_index`, `search_readiness`, the monitor search and
  memory clients (request and SSE), and `embedder_client::RemoteEmbedderClient`
  all route through it. Public-internet clients — `update`, `chat`,
  `inference`, `openrouter_legacy` — deliberately still honour the operator's
  proxy (#4392).
