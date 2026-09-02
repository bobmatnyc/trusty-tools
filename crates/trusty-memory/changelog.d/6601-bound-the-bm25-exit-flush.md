Fixed

- `transport::uds::serve_with_shutdown` awaits the BM25 exit flush under
  `trusty_common::shutdown::CLEANUP_RESERVE` (#6601 review). The reserve is the
  time `serve_until`'s drain holds back so the work after it can run, and
  `bm25_lane::shutdown` claimed there was "no window in which a SIGKILL can land
  mid-flush" — but `flush_all` takes the residency mutex and flushes every
  resident palace with no deadline. A slow flush spent the whole reserve, the
  socket unlink after it never ran, and the SIGKILL left behind the stale socket
  file `bind_singleton_hardened` exists to work around.
- An abandoned flush warns, naming the budget, and costs nothing a SIGKILL would
  not have: `BM25Index::flush` renames a temp file into place, so every palace
  keeps the snapshot its last coalescing tick published.
