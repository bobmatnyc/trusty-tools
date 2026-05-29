# ADR 0001: SQLite Tuning Pragmas

- **Status**: Accepted
- **Date**: 2026-05-11
- **Deciders**: trusty-git-analytics core team

## Context

`tga` is a single-process CLI tool that uses SQLite as its sole persistence
layer. The workload profile is:

- **Heavy write bursts** during the `collect` stage (tens of thousands of
  commit rows inserted in a few seconds).
- **Mixed read/write** during `classify` (read commits, write classification
  verdicts).
- **Read-only** during `report` (large table scans aggregated into CSV /
  Markdown).
- **Single process, single thread of SQL access at a time** — pipeline stages
  run sequentially, and the `Connection` is held behind `&mut` in
  `Database`. No threads share the connection.

SQLite ships with conservative defaults aimed at general-purpose use on
constrained hardware. For an analytics CLI running on a developer laptop
those defaults leave significant performance on the table: a 2 KB page
cache and synchronous full-fsync on every commit can dominate the
runtime of `tga collect` against a busy monorepo.

We also evaluated whether to introduce a connection pool
(`r2d2-sqlite`) to manage concurrent access. The answer is no — see
"Decision" below.

## Decision

Every SQLite connection opened by `Database::open` (and the in-memory
variant used by tests / `--dry-run`) has the following pragmas applied
immediately after open, before migrations run:

| Pragma | Value | Reason |
|--------|-------|--------|
| `journal_mode` | `WAL` | Mandated by project conventions. Allows concurrent reads during writes and avoids long writer locks on big inserts. |
| `synchronous` | `NORMAL` | Together with WAL, gives the same crash-safety guarantees as `FULL` for committed transactions while halving the fsync cost. Loss is bounded to the most recent uncommitted transaction. |
| `foreign_keys` | `ON` | Enforces our schema-level FK relationships (e.g. `commit_work_items → work_items`). Off by default in SQLite for backwards compatibility — turning it on is non-negotiable for correctness. |
| `cache_size` | `-65536` | 64 MB page cache (negative value = absolute KB). Large enough to keep the working set of a typical week-by-week classification run resident; small enough that 2 GB-RAM CI hosts are unaffected. |
| `temp_store` | `MEMORY` | Holds temporary indexes and intermediate sort buffers in RAM rather than spilling to `$TMPDIR`. Significantly speeds up the report stage which issues `ORDER BY` over large joins. |
| `mmap_size` | `268435456` | 256 MB memory-mapped I/O window. SQLite uses `mmap` for read-only access to pages, bypassing `read()` syscall overhead. The OS will only pull in pages we actually touch, so on small databases the cost is zero. |

**No connection pool.** `tga` opens exactly one connection per process
and stages run sequentially. A pool (`r2d2-sqlite`, `deadpool-sqlite`,
…) would introduce locking and a synchronization layer with no
parallel workload to amortize it against. If a future stage genuinely
needs parallel SQL access, the preferred pattern is to open additional
`rusqlite::Connection` instances per worker thread via
`Connection::open_with_flags(SQLITE_OPEN_READ_ONLY | ...)` — each
worker gets its own pragmas applied via the same
`Database::apply_pragmas` helper.

## Consequences

### Positive

- `tga collect` write bursts no longer block on fsync — observed end-to-end
  speedup of ~3× on a 50k-commit monorepo (anecdotal; not yet benchmarked).
- `tga report` table-scan + sort phases stay entirely in RAM for databases
  up to a few hundred MB.
- Memory footprint stays bounded: 64 MB page cache + 256 MB mmap window
  (most of which is on-demand) is well within the budget of any developer
  machine or CI runner.
- FK enforcement catches link-table bugs at write time rather than producing
  silent orphaned rows.

### Negative

- A power loss during a write transaction with `synchronous = NORMAL` can
  in theory lose the most recent transaction (but never corrupt the DB —
  WAL guarantees that). For an analytics tool whose entire dataset can be
  re-derived from git history, this is an acceptable trade.
- The `cache_size = -65536` setting reserves 64 MB of RSS per connection.
  At one connection per process this is fine; if we ever add a pool it
  becomes a multiplier and the value must be revisited.
- `mmap_size = 256 MB` reserves virtual address space, not RSS. Harmless
  on 64-bit platforms (`tga` does not support 32-bit).

### Neutral

- The pragma set is centralized in `Database::apply_pragmas`; future
  changes touch exactly one function and apply uniformly to in-memory test
  DBs and on-disk production DBs alike.
- Tests assert that `journal_mode()` returns the expected value after open,
  so a regression in pragma application would be caught immediately.

## References

- SQLite WAL journal: <https://www.sqlite.org/wal.html>
- `PRAGMA synchronous`: <https://www.sqlite.org/pragma.html#pragma_synchronous>
- `PRAGMA cache_size`: <https://www.sqlite.org/pragma.html#pragma_cache_size>
- `PRAGMA mmap_size`: <https://www.sqlite.org/pragma.html#pragma_mmap_size>
- `r2d2-sqlite`: <https://docs.rs/r2d2_sqlite/> (evaluated and not adopted)
