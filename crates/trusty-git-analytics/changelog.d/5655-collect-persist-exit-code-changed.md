Changed

- `CollectionStats::errors` is now `Vec<CollectionFault>` rather than `Vec<String>`, and `CollectionStats::stage_failures()` returns the subset whose data is missing. `CollectionFault` renders as its message, so existing `{e}` formatting is unchanged.
