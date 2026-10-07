Added
- `bm25::effective_corpus_cap()` returns the BM25 corpus cap upserts enforce right now (the `TRUSTY_BM25_CORPUS_CAP` override, else the 50 000 default), so a consumer can report which cap truncated its corpus (#9235).
