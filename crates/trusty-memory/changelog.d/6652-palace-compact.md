Added
- `trusty-memory palace stats <name>` (#6652) — read-only report of a palace's `kg.redb`: file size, per-table row counts and byte usage, the active-vs-history triple split, superseded-drawer count, and a reclaimable estimate. Safe against a live palace with the daemon running.
- `trusty-memory palace compact <name> [--dry-run]` — prune stale history rows and rewrite `kg.redb` to reclaim disk.
- `palace_dream` accepts `compact: true` (and `dry_run: true`), returning a `compaction` object with before/after byte counts and pruned-row counts.
- `trusty-memory doctor` reports the largest `kg.redb` on disk: warns at 100 MB, fails at 500 MB.
- `[dream]` config keys `compact`, `prune_history_after_days`, `compact_min_bytes`, `compact_keep_backup` in `~/.trusty-memory/config.toml`.
