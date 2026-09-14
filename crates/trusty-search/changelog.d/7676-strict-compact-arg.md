Fixed
- A non-boolean `compact` on `search`, `search_all`, `search_lexical`, `search_semantic`, or `search_kg` is now rejected with `InvalidParams` instead of being read as `false`. `compact: "true"` used to return full hits byte-identical to a call that never asked for compaction (#7676).
