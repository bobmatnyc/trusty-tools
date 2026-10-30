Removed

- Test-only HTTP handler shims for the create-index, relocate-index and reindex routes, the last of the #9939 shims; their tests now call `create_index_report`, `relocate_index_report` and `reindex_report`, the cores the socket serves (#9939, no runtime change).
