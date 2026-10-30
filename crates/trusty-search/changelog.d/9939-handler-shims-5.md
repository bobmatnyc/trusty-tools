Removed

- Test-only HTTP handler shims for the single-index and global search routes; their tests now call `search_report` and `global_search_report`, the cores the socket serves (#9939, no runtime change).
