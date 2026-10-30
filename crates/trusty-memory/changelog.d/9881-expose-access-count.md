Added
- `memory_recall`, `memory_recall_all` and `memory_list` rows now carry `access_count`, `accumulated_boost` and `last_accessed_at` (#9881). The fields are additive. A recall row shows the count as of before its own access.
