Fixed

- MCP tools now surface a daemon `503` as a structured error instead of a prose
  string (#5350). A cold-parked, restore-failed, corpus-unavailable, or
  vector-unavailable index used to reach an MCP client as
  `POST <url> returned 503 Service Unavailable: {…}`, so the `error` code,
  `retryable` flag, and `restore_via` hint added in #5345 survived only as text.
  `tools/call` now carries the daemon body verbatim in `_meta` under
  `error_code: "INDEX_UNAVAILABLE"`, and the bare-method form carries it in
  `error.data` under the new JSON-RPC code `-32012`. Applies to every verb
  (`search`, `index_status`, `list_chunks`, `index_file`, `remove_file`,
  `get_call_chain`, `delete_index`). A 503 with no JSON body, or any other
  status, is unchanged.
