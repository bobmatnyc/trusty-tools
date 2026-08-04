Fixed

- `sys_metrics::dir_size_bytes` can no longer abort the calling process. The
  walk was recursive, so descending N levels held N `ReadDir` handles open at
  once; when `std`'s `impl Drop for DirStream` hit a failing `closedir(3)` and
  panicked, the unwind ran the enclosing handles' destructors, a second
  `closedir` failed the same way, and a panic raised during unwinding is a
  non-unwinding panic Rust aborts on unconditionally. The walk is now
  iterative and holds at most one directory handle at a time — removing the
  second destructor from the unwind path — and is additionally wrapped in
  `catch_unwind`, which returns the partial byte total instead of propagating.
  This took down the `trusty-search` daemon 40 times in a week, roughly every
  7 minutes under load (#4764)
- The size walk is now bounded: it refuses to descend past 64 levels and
  abandons a walk that exceeds a 30 s wall-clock budget, reporting the partial
  total in both cases. A best-effort disk figure should never become an
  unbounded sweep of an actively-mutating tree (#4764)
