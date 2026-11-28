Added
- `memory_recall`, `memory_recall_deep` and `memory_recall_all` take an optional `read_only` boolean (default `false`). A read-only recall returns the same ranking but records no access: `access_count` and `last_accessed_at` stay unchanged on the recalled palace and on every rulings palace whose ruling it folds in (#11348).
