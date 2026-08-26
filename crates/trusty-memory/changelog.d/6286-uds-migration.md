Changed

- **The daemon serves one hardened Unix socket instead of a TCP port.** `127.0.0.1:7070`, the `7070..=7079` walk, the OS-assigned fallback, both `http_addr` discovery files, the same-origin write guard and the `/sse` broadcast are all gone; the daemon binds `trusty_common::daemon_socket_path("trusty-memory")` and every consumer derives the same path ([#6286](https://github.com/bobmatnyc/trusty-tools/issues/6286), ADR-0032)
  - `transport::rpc::dispatch` already routed ~75 method names, so it mounts whole as the router's `RpcFallback` rather than being re-listed method by method. `transport::uds::FOLDED_METHODS` is only what the fallback does not cover — the ~20 endpoints that existed as axum routes and nothing else
  - `memory.chat` streams. It is what `trusty-common`'s multi-frame UDS extension was built for: the chat handler pushes LLM tokens as the model produces them, and a mid-stream provider failure now ends the stream with the terminal ERROR frame instead of a `data: {"error"}` line followed by a normal end, which a reader could not tell from a finished answer
  - a stale `http_addr` from a pre-#6286 install is deleted at every start, so `tctl`'s bootstrap guard cannot refuse an install over a port nothing binds. The deletion skips the `$HOME` dotfile when `TRUSTY_DATA_DIR_OVERRIDE` is set, for the reason #880 gave for skipping the write
- **`serve --http` is accepted and ignored.** A launchd plist installed before ADR-0032 passes it, and under `KeepAlive` a clap usage error would crash-loop the daemon rather than start it. Any address given is discarded with a warning
- **`monitor web` no longer opens a browser.** It pointed at this crate's own SPA at `http://<addr>/ui`; the dashboard mounts on `trusty-console`, and the subcommand now says so rather than opening a URL nothing answers. The embedded assets stay in the binary for that mount
- **The `axum-server` feature is renamed `daemon`.** It gated the same thing it always did — the serving surface a slim library consumer does not want — but named a dependency this crate no longer has. `default-features = false` is unchanged, so `trusty-agents` needs no edit

Removed

- `run_http`, `run_http_dynamic`, `run_http_on`, `bind_dynamic_port`, `http_addr_path`, and the `web` and `foreground` modules. `transport::uds::serve` and `transport::uds::socket_path` replace them
- The `/api/v1/recall`, `/api/v1/kg/prompt-context`, `/api/v1/kg/aliases`, `/api/v1/kg/prompt-facts`, `/api/v1/kg/gaps`, chat-session CRUD and `POST /api/v1/activity/hook` routes retired without a folded equivalent: every one duplicated a method `transport::rpc::dispatch` already served
- `DEFAULT_HTTP_PORT` survives as a compile-time stub only, for `trusty-agents`' unmigrated REST client. Nothing binds 7070

Fixed

- **The MCP stdio bridge would have started refusing requests that omitted `jsonrpc`.** `trusty_mcp::Request` serialises the field as `null` when a client omits it; `POST /rpc` never checked it, and `RpcRouter` refuses anything that is not exactly `"2.0"`. The bridge normalises it before forwarding, so a client that worked before the migration works after it
- **A `tools/call` naming a streaming method is refused with a reason.** MCP stdio writes one response per request, so a token stream had no shape to arrive in — the two silent outcomes were returning the first token as the answer, or hanging
