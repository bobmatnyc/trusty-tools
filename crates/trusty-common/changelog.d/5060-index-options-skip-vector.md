Added

- `search_index::IndexOptions` and `ensure_project_indexed_with`, so a caller can register a trusty-search index with its vector lane suppressed (`skip_vector: true` — BM25 and KG only, no embeddings) ([#5060](https://github.com/bobmatnyc/trusty-tools/issues/5060))
  - `ensure_project_indexed` is now a one-line wrapper over it; `IndexOptions::default()` reproduces the previous behaviour exactly, so no existing caller changes
  - `POST /indexes` now always carries a `skip_vector` field; `false` is equivalent to omitting it
