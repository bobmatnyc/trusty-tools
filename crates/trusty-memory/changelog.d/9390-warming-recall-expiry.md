Fixed
- `memory_recall`, `memory_recall_deep` and `memory_recall_all` no longer return a drawer past its `expires_at` while the embedder is warming. The BM25 lexical lane hydrated its hits without the expiry check the other recall layers apply, so an expired Tier C slot matching the query came back. The drawer is skipped, not deleted; `memory_list` still lists it (#9390).
