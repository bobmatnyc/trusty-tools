Changed
- The HNSW op-budget watcher reads its start time from tokio's clock instead of `std::time::Instant` (#10335). Behaviour is unchanged: tokio's clock is the system clock unless a test pauses it. The op-budget progress tests now run on a paused clock and no longer trip a progressing group when the host is under load.
