Added

- **OKG index journal + reconcile (`okg::index_journal`, #3892):** a second
  append-only journal per source, `_sources/<id>.index.jsonl`, recording which
  entities reached the store's bound trusty-search index and at what content
  hash, plus `KbStore::okg_index_backlog` — the pure diff of that journal
  against the item ledger. This is what makes "ingested" and "searchable" two
  separately reportable facts instead of one silently-conflated claim. The item
  ledger is deliberately NOT overloaded with index state: an index push failure
  must not make ingestion non-convergent when the search daemon is merely down,
  and an entity the ledger skips as unchanged must still be re-pushed if it
  never landed. Staleness is decided by the entity file's content hash (a
  `volatile` item's fingerprint carries no information), short-circuited by a
  `(size, mtime)` check so a settled tree costs one `stat` per entity. The push
  itself lives in `trusty-agents` — this crate stays deterministic and
  network-free.
- **`okg_sources` reports search coverage:** each row carries `index.synced` /
  `index.pending` alongside its watermark, and `KbStore::okg_index_coverage`
  folds the same numbers tree-wide. "Ingested but not yet searchable" is now a
  visible state rather than an invisible one.
- **OKG builder engine (`okg` module):** the machinery that GROWS a KB tree from
  real sources, idempotently and additively. Three durable guarantees, all
  unit-proved:
  - *Idempotent* — re-running an ingest over an unchanged corpus writes nothing
    (zero new entities, zero file touches).
  - *Additive* — registering a new source appends a row and starts a fresh
    ledger without disturbing any other source's entries; widening an existing
    source (e.g. a Gmail window reaching further back in time) updates its
    locator in place and ingests only the items its ledger has never seen.
  - *Crash-convergent* — the ledger is journaled per item (flush + `sync_data`
    before the next item), so a killed run re-runs to the same state.
- `okg::registry` — `_sources/registry.toml`, a human-readable, hand-editable,
  commit-safe source list. The locator is an externally tagged enum, so the TOML
  sub-table name (`doc_store` / `gmail` / `drive`) *is* the source kind and the
  two can never disagree. Source ids are slugged, so an id can only ever name
  one path segment. Saves are write-if-changed.
- `okg::ledger` — `_sources/<id>.jsonl`, an append-only per-item journal keyed by
  the source's native item id. `is_current(item_id, fingerprint)` is the single
  skip predicate: unchanged → skip, changed → re-ingest and replace, tombstoned
  → re-ingest (a returning item is never ignored forever). A torn final line
  from a crash costs exactly one item and is counted, not fatal; the next append
  heals the line break first so the new record is not swallowed with it.
  `Watermark` (item counts, oldest/newest coverage, last run) is derived from the
  journal rather than stored, so it cannot drift from what was ingested.
- `okg::ingest` — the fetcher-agnostic engine. `KbStore::ingest_items` takes
  normalised `SourceItem`s and writes entities through the existing
  deterministic `put_entity` path (overwrite mode, so a re-ingest replaces
  rather than accretes). A per-item failure is reported and the run continues.
  Vanished items are surfaced in `missing`, or tombstoned (`source_status:
  deleted` + `tombstoned:`, body preserved — never a deletion) when the caller
  can prove the source's full corpus was enumerated.
- `okg::policy` — **read-side confinement for doc stores.** `DocStorePolicy`
  holds an operator-configured allow-list of ingestible roots and additionally
  refuses any HIDDEN path below a root. "Hidden" spans three rules, because one
  platform's convention is not another's: a dot-prefix (`~/.ssh`, `~/.aws`,
  `~/.gnupg`, `~/.config/gh`), the macOS `UF_HIDDEN` file flag, and a `Library`
  name backstop for when that flag is absent. The flag rule matters because
  `~/Library` — holding `Application Support/<App>` tokens, `Keychains`, and
  `Preferences` — is **not** dot-prefixed, so a dot-only rule would have left it
  fully readable under the default `$HOME` policy. It resolves symlinks and `..`
  before judging, and an unconfigured policy denies everything rather than
  defaulting open. Scoped below the matched root, so an operator who names a
  directory inside hidden territory in `docstore_roots` still opts in explicitly.
  Enforced inside `scan` — at the point the filesystem is touched — so a
  hand-edited `registry.toml` row pointing at a credential directory is refused
  on every run, not just at registration. The hidden-flag probe fails CLOSED: a
  permission-denied stat, a transient IO error, or a segment removed mid-check
  counts as hidden, because "cannot determine" must never read as "safe".
  Known gap, tracked in #3889: validation is by path and the walk re-resolves
  it, leaving a TOCTOU window against a co-located same-user adversary; closing
  it needs fd-relative (`openat`) traversal.
- `okg::docstore` — the in-crate filesystem fetcher: deterministic sorted walk,
  extension filtering, SHA-256 content fingerprints (path and mtime both lie),
  graceful binary skipping (reported by name, never fatal), and deterministic
  chunking of oversized documents into part-entities that share the whole-file
  hash. `KbStore::okg_ingest_docstore` runs scan + ingest end to end.
- `KbStore::okg_register_source` / `okg_source` / `okg_sources` — registration
  and the per-source status view.
