Fixed
- A palace open can no longer hang while it rebuilds the vector index. The workspace now builds a patched `hnsw_rs` 0.3.4 that releases a lock before it updates neighbouring points, so parallel replay stays fast and cannot deadlock (#10682).
