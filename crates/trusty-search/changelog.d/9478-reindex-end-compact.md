Fixed
- A staged reindex that leaves the HNSW graph due for compaction now starts the background rebuild when it ends, so the graph is rebuilt once writes stop even with `TRUSTY_HNSW_DEMOTE_COOLDOWN_SECS=off` or `TRUSTY_HNSW_REVIEW_IDLE=off`. Before, nothing rebuilt it until a later write ([#9478](https://github.com/bobmatnyc/trusty-tools/issues/9478))
