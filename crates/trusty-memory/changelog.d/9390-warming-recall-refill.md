Fixed
- While the embedder is warming, `memory_recall`, `memory_recall_deep` and `memory_recall_all` fetch more BM25 hits when expired Tier C slots fill the lexical window, up to the same 1024-hit cap as the vector lanes. Before, 20 or more expired slots matching the query hid the live slot. Nothing is deleted (#9390).
