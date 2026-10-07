Added
- `search.health` over the socket and `GET /health` now report the transport the daemon serves, as `transport: {socket_path, http_addr}`: the Unix socket path and the HTTP address the daemon actually bound. A field is `null` when that listener is not bound, never a guessed default. The field is additive; no existing key changes (#9030).
