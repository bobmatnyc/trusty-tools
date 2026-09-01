Fixed

- A file whose chunks collide on id is indexed and searchable again. `make_chunk_id` omits the end line, so a minified JS bundle — one line, many single-letter declarations — produced repeated ids. `UsearchStore::upsert_batch` resolved one key for all of them, added the first, failed every later one with usearch's "Duplicate keys not allowed in high-level wrappers", then rolled back the id-to-key mapping the successful add had installed, leaving a vector nothing could reach (#6571).
  - `chunk_ast` now drops a chunk whose id an earlier chunk in the same file already claimed, so the duplicates are never embedded.
  - `upsert_batch` collapses duplicate ids at the boundary that owns the one-vector-per-id contract.
  - The skip warning reports the error the add actually returned instead of asserting a NaN or zero vector it did not observe.
