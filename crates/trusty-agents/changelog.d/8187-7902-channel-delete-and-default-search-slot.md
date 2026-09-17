Added

- `DELETE /api/channels/{id}` removes one global channel under the same
  channel-write credential and revision compare-and-swap as the create and the
  update ([#8187](https://github.com/bobmatnyc/trusty-tools/issues/8187)). It
  refuses with `409` naming the assistants whose bindings still overlay the
  channel unless the request carries `force=true`, in which case those bindings
  are left on disk and reported as `inert_bindings`.
- `GET /api/agents/{name}/stores` reports a `search_slot` object — the declared
  primary index, the index an unqualified `vector_search` actually answers
  from, the protected extraction index, and whether the two disagree
  ([#7902](https://github.com/bobmatnyc/trusty-tools/issues/7902)). A slot that
  cannot be resolved is reported as an `error` beside a store that still reads
  as connected, rather than staying invisible.
