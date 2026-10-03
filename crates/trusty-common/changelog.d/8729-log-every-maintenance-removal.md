Changed
- Every drawer a maintenance pass deletes (dream dedup, content prune, prune, room consolidation, TTL purge) is now logged on its own `warn` line naming the palace, drawer id and reason, beside its journal record. Before, the log showed only a per-pass count (#8729).
