Fixed

- **An index whose durable corpus failed to open kept accepting watcher
  writes, permanently destroying the corpus (issue #4122, P0 data loss).**
  When `CorpusStore::open` failed at load time the loader set
  `corpus_open_failed` but left the handle fully live, watcher included, so
  ordinary unrelated file saves rebuilt a fresh PARTIAL corpus over the
  never-opened original — in production `chunk_count` climbed `0 → 68 → 1334`
  and the index came back "healthy" on the next restart holding the wrong
  content. A `corpus_open_failed` index is now **write-quarantined**: every
  incremental write path (`service::watch_loop`, `service::reconcile`, and
  `POST /indexes/{id}/index-file`) is refused at the shared
  `CodeIndexer::index_file` choke point, and the watcher additionally bails
  before it reads and chunks the saved file. Refusals are emitted at ERROR
  (the only level `trusty_common::error_capture` persists to `errors.jsonl` /
  `list_recent_errors` / `tm doctor`), throttled to the 1st and every 100th,
  and counted on `CodeIndexer::refused_incremental_writes`. **Recovery is a
  daemon restart** — only a successful `CorpusStore::open` lifts the
  quarantine and it is attempted solely at load time, so the ERROR text says
  so explicitly and warns that a reindex neither clears the state nor
  persists anything (with no corpus wired it skips staging entirely). The
  clean-restart path (the incident's `cto-duetto`, restored at its full
  200,090 chunks) is unaffected. Bulk reindex is deliberately not gated,
  which is safe because a quarantined index holds no `CorpusStore` — an
  invariant now documented and pinned by `debug_assert!`s, since boot
  reconcile auto-fires reindexes with no quarantine check. Reads are
  unchanged; corpus-failed indexes still return empty results (that is issue
  #4087, not fixed here).

- **`POST /indexes` reported every stage `Pending` even when a colocated corpus
  restore succeeded** ([#4110](https://github.com/bobmatnyc/trusty-tools/issues/4110)).
  `create_index_handler` doubles as the "adopt an existing colocated corpus"
  door — `build_indexer_from_entry` synchronously restores the redb corpus, the
  HNSW snapshot and the symbol graph — but the handler then set
  `lexical: pending(), semantic: pending()` unconditionally and threw that
  outcome away. Since `search_capabilities` is derived from `stages`, a fully
  intact index came up advertising no vector lane: semantic search hard-errored
  with "requires Stage 2 (embeddings), which is not yet ready" and `search_all`
  silently degraded to BM25-only, every hit reporting `match_reason="bm25"` —
  indistinguishable from a genuinely dead vector lane. Only a daemon restart
  cleared it, because the warm-boot path already classified correctly from the
  same signals. The registration path now derives stages with
  `derive_warm_boot_stages`, the same pure classifier warm-boot and
  lazy-restore use, over the same signals read the same way, so the two can no
  longer disagree about an identical on-disk state. A genuinely new index still
  reports `created` (lexical `Pending`), not warm-boot's `walking`.

---
