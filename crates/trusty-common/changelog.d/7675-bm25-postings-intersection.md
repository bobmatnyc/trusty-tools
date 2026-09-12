Added

- `BM25Index::docs_containing_all` returns the document ids whose postings carry every term in a list, intersecting the inverted index at `O(min df)` instead of scanning the corpus. trusty-search's exact-match floor uses it to generate candidates for a verbatim literal rather than matching a regex against every chunk's content (#7675).
