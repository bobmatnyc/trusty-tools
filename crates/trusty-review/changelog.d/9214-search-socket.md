Changed

- trusty-review reaches trusty-search over its Unix socket when one is
  present, and over HTTP otherwise. `TRUSTY_SEARCH_SOCKET` wins; an explicit
  `TRUSTY_SEARCH_URL` (or a non-default `search_url`) keeps HTTP; with neither
  set, the default socket is used when its file exists. The default socket
  follows the daemon's own rule: `<TRUSTY_DATA_DIR>/trusty-search.sock` when
  `TRUSTY_DATA_DIR` is set, so an isolated instance never reads the shared
  daemon. A socket file with no
  daemon behind it is reported as unreachable and never falls back to HTTP.
  Socket errors read as the HTTP ones did: an unknown index is a 404 and an
  unavailable one is a 503 carrying the daemon's body (#9214).
- The search, report-trace, index-registry and subprocess-analyze clients all
  follow that rule, and the spawned `trusty-analyze review` child is given the
  resolved transport as `TRUSTY_SEARCH_SOCKET` or `TRUSTY_SEARCH_URL`. Context
  gate messages name the transport used (`socket <path>` or the URL) (#9214).
- New, additive: `SearchTransport`, `HttpSearchClient::with_transport` and
  `transport`, `SubprocessAnalyzeClient::with_transport`,
  `HttpTraceSource::with_transport` and
  `index_registry::fetch_registered_indexes_via`. Existing constructors keep
  their signatures (#9214).
