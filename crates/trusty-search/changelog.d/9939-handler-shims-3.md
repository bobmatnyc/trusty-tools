Removed

- Test-only HTTP handler shims for the index-status, graph, list-indexes and delete-index routes; their tests now call the `*_report` cores the socket serves (#9939, no runtime change).
