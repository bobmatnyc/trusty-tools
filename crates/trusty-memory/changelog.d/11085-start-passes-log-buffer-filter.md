Fixed
- Linux: `start` and `restart` pass `RUST_LOG_BUFFER` to the daemon they spawn, so its in-memory log buffer keeps the filter a shell-started daemon would use (#11085).
