Fixed
- Under redb 4.3, a table iterator keeps returning an error after its first one. Five trusty-memory readers discarded that error and now report it (#8254).
- `ActivityLog::list` no longer spins forever or returns a truncated feed as success when a row cannot be read.
- `ActivityLog::prune` no longer collects an empty eviction batch on a read error, which left its loop spinning on an unchanged row count.
- `backfill-report` records a palace whose room registry cannot be read as failed, instead of listing every drawer under a short id.
- The no-palace index marks a palace `unreadable` when its wing or room registry cannot be read, instead of reporting zero wings or no rooms.
