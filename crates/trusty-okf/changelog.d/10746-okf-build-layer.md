Added
- `build` (SPEC-OKF-01 §2.7.2, #10746): the source registry, ledger, JSONL journal, ingest engine, doc-store scanner, read policy and trust labels, moved from trusty-kb. The `_sources/registry.toml` and `_sources/<id>.jsonl` formats are unchanged; a registry written by trusty-kb, `gmail` and `drive` rows included, loads and saves byte for byte.
- `build::registry::Locator::Connector { kind, params }`: every source kind except the doc store, kept with its keys in file order. trusty-okf defines no connector variant (B6).
- The ingest engine writes through `Bundle::put`: a re-ingest keeps `status`, `supersedes`, `superseded_by`, `stale_after` and the verification keys (§2.9), a content change moves `verified[]` to the verification record (§2.10), and `generated.by` is `process:trusty-okf-ingest`.
- `format::protected` (§2.13.1): a protected key from a fetcher field, an extractor field or a source file's frontmatter moves unchanged under `source_fields`, and the ingest report lists one `quarantined_key` finding per key and item. `SourceItem::frontmatter` carries structured source frontmatter.
- `OkfError::PolicyRefused` for a doc-store directory the read policy refuses.
