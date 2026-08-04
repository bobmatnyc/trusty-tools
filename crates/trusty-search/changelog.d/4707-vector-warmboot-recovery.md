Fixed

- An index whose vector layer warm-booted empty is no longer left permanently
  broken while still self-reporting `ready`. Two defects combined: the vector
  layer never recovered, and the status surface never admitted it
  ([#4707](https://github.com/bobmatnyc/trusty-tools/issues/4707)).
  - **Recovery.** When warm-boot's `UsearchStore::load_from` discards a snapshot
    (the `#2922` size floor, a corrupt sidecar, the `#3970` torn-pairing guard)
    the store falls back to a fresh empty one. Every later save of that empty
    store was then correctly refused by the `#1711` data-loss guard — and
    nothing further happened, so the index served zero vectors forever with an
    intact snapshot sitting on disk. After refusing the write, `save()` now
    adopts that on-disk snapshot, so the vector lane recovers without a reindex.
    The `#1711` guard is unchanged and nothing is ever written on that path;
    adoption only moves in-memory state towards what is already durable, and
    reuses `load_from`, so a truncated or torn snapshot is rejected by exactly
    the code that rejects it at warm-boot. The `#1717` shrink refusal
    deliberately does not recover this way — a partial in-memory index may hold
    vectors disk does not.
  - **Honest health.** The semantic stage is no longer published as `ready`
    when the live vector store holds zero vectors, a corpus exists, and an
    embedder is wired. An all-hash-skipped incremental reindex legitimately
    embeds nothing (`#868`), and both the fast pass and the deferred-embed pass
    marked `ready` on that basis without ever consulting the store they were
    vouching for. The stage now reports `failed` with an actionable reason, so
    `search_capabilities` stops advertising `vector` and the search handler
    keeps down-shifting queries to the working lexical lane instead of routing
    them through a query-embed step whose failure surfaced as
    `500 internal search error` on every query.
