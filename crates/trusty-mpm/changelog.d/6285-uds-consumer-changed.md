Changed

- `tm doctor`'s `search` and `search_index_pin` checks call the trusty-search
  daemon over its Unix socket (#6285, ADR-0032) — `search.health`,
  `search.indexes.list` and `search.index.status` in place of `GET /health`,
  `GET /indexes` and `GET /indexes/{id}/status`. Both derive the socket through
  `trusty_common::daemon_socket_path("trusty-search")`, the same call the daemon
  binds, so there is no address to discover and no stale `http_addr` to disagree
  with.
- The daemon's startup banner names the trusty-search socket path instead of a
  port read off disk.
- `search_index_pin` reports a pin the daemon does not hold off the JSON-RPC
  not-found code where it used to read a 404 status; another refusal is reported
  with the daemon's own code rather than an HTTP status.
