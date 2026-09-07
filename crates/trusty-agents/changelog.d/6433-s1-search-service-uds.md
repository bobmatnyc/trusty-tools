Breaking

- The per-project search-as-a-service daemon (`--search-service`) serves a
  hardened Unix socket instead of loopback TCP (#6433, ADR-0032). The five
  `/search/*` HTTP routes become the JSON-RPC methods `search.health`,
  `search.query`, `search.index_file`, `search.remove_file` and
  `search.reindex`, on `~/.trusty-agents/sockets/<project>.search.sock`.
  - The `.trusty-agents/state/search.pid` discovery file is retired and deleted
    at every start: caller and daemon derive the same socket path from the
    project root, so there is no address to publish. Anything reading that file
    for a port must call `SearchDaemonClient::connect_if_running` instead.
  - `search.health` drops the `indexed_chunks` field, which always held the
    sentinel `-1`.
  - `index_file` and `remove_file` both answer `{"chunks": N}`; the HTTP routes
    spelled the second one `removed`.
  - `--search-service` in `--help` describes the socket and the five methods; it
    named the five HTTP routes.
