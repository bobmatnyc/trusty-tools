Changed

- The managed-session search-index GC and the destructive index-delete capability
  call the trusty-search daemon over its Unix socket (#6285, ADR-0032) —
  `search.indexes.list`, `search.index.status` and `search.index.delete` in place
  of `GET /indexes?details=true`, `GET /indexes/{id}/status` and
  `DELETE /indexes/{id}?delete_data=true`. Both route through
  `daemon::search_rpc`, this crate's one trusty-search client, so there is no
  second address resolver and no second HTTP client to keep in step.
- `DestructiveIndexDelete` refuses to hand out the capability when no daemon
  socket is bound, which replaces the old "no `http_addr` discovery file" refusal
  at the same point — before any request is built, and still after the #4743
  test-process refusal that is checked first.
