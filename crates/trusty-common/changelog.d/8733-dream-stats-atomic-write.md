Fixed
- `PersistedDreamStats::save` now publishes `dream_stats.json` through `atomic_file::write_atomic` (sibling temp file, `fsync`, rename) instead of a truncating `fs::write`. A concurrent `load` no longer reads an empty or partial file (`EOF while parsing a value`), and a failed save leaves the previous snapshot intact (#8733).
