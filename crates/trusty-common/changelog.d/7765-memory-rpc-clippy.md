Fixed

- `cargo clippy -p trusty-common --features memory-rpc --all-targets -- -D
  warnings` no longer fails on dead code in `uds_mock.rs`. `BlockingMockDaemon`
  and `spawn_blocking_at` are used only by `search_index`'s synchronous test
  rigs, which live behind the `search-index` feature; `memory-rpc` pulls in
  the whole `uds_mock` module (via `uds`) without pulling in `search-index`,
  so those two items were unconstructed. They now carry their own
  `#[cfg(feature = "search-index")]` gate (#7765).
