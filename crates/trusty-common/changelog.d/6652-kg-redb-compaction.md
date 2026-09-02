Added
- `kg.redb` copy-then-swap compaction (#6652). `memory_core::dream::kg_compact_pass` measures the palace's knowledge-graph store read-only, prunes closed `hist:` triple rows older than `dream.prune_history_after_days`, and rewrites the file into a fresh sibling that is renamed into place under the palace write mutex. redb never returns freed pages to the filesystem, so before this the store only ever grew.
- `store::kg_redb::KgRedbStats` — a read-only per-table measurement (row counts, stored/metadata/fragmented bytes, the active-vs-history triple split, and a reclaimable estimate). Opens the file `O_RDONLY`, or a throw-away snapshot when a writer holds it, so it never writes to the store it measures.
- `store::ReadOnlyRedb` — a redb open that provably cannot write to the live file.
- `DreamConfig` gains `compact`, `prune_history_after_days`, `compact_min_bytes` and `compact_keep_backup`; `DreamStats` gains `kg_bytes_reclaimed`, `kg_bytes_after` and `kg_history_rows_pruned`.
