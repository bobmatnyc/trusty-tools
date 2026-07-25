# Changelog

All notable changes to `trusty-kb` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/); this crate uses
independent semantic versioning per the workspace convention.

## [Unreleased]

### Added

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
- `okg::docstore` — the in-crate filesystem fetcher: deterministic sorted walk,
  extension filtering, SHA-256 content fingerprints (path and mtime both lie),
  graceful binary skipping (reported by name, never fatal), and deterministic
  chunking of oversized documents into part-entities that share the whole-file
  hash. `KbStore::okg_ingest_docstore` runs scan + ingest end to end.
- `KbStore::okg_register_source` / `okg_source` / `okg_sources` — registration
  and the per-source status view.

### Changed

- `collection_dirs_on_disk` now skips every `_`-prefixed directory rather than
  only `_state`, so store machinery (`_sources`) is never mistaken for a free
  topic collection and never gets a generated `index.md`. `kb_convert_tree`'s
  walk skips the same prefix.

- Initial crate: a native MCP stdio server (`trusty-kb serve --stdio`) that
  maintains a personal-knowledge-base markdown tree deterministically. Built on
  trusty-common's shared native-MCP framework (`run_stdio_loop` +
  `initialize_response`, protocol `2024-11-05`); mirrors the `tagent mcp-serve`
  shape (static `tools/list`, `isError:true` envelopes, stderr tracing before
  the loop). No LLM calls — pure, deterministic file mechanics.
- Schema module implementing the **OKF v0.1 container + snake_case
  schema.org/FOAF relationship profile**: `type` is the only required field;
  `index.md`/`log.md` are reserved (non-entity) files; unknown frontmatter keys
  are preserved verbatim. Six folder-per-type collections (people,
  organizations, places, events, projects, things) with a shared base envelope
  and per-collection relationship edge fields. Pluggable — the whole profile
  lives in one module.
- Inverse-edge reconciler: writing one side of a relationship
  (parent_of↔child_of, works_at↔employee, symmetric `knows`/`spouse_of`/…)
  deterministically materialises the mirror edge on the link target; idempotent
  across re-runs.
- Tools: `kb_status`, `kb_list_trees`, `kb_put_entity` (create-or-merge, sorted
  keys, change-detected `updated`, unknown-key-preserving merge, idempotent body
  append), `kb_get_entity`, `kb_list`, `kb_ensure_structure` (generates
  `index.md` READMEs deterministically), `kb_validate` (parse errors, missing
  `type`, dangling wiki-links, slug/filename mismatch, duplicate aliases), and
  `kb_convert_tree` (arbitrary tree → collection model, `report_only` default,
  provenance recorded, never destroys content, byte-stable/idempotent).
- Multi-tree service model: one instance serves every assistant's tree. Every
  tool resolves its root per call (explicit `root` → `agent`-mapped
  `<knowledge_dir>/<agent-slug>` → service default), with path confinement +
  symlink-escape rejection under the knowledge directory.
