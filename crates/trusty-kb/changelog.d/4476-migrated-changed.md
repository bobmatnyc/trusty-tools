Changed

- `KbStore::okg_sweep_deleted` separates the deletion sweep from `ingest_items`,
  so a source ingested page by page can commit each page and then run deletion
  detection ONCE against the complete observed id set. A per-page sweep would
  have tombstoned every item outside the page. `IngestReport::merge` folds the
  per-chunk reports into one summary.
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
