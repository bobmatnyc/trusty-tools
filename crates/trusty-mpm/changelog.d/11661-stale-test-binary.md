Fixed
- `tm build-lease` now fails a `cargo test` that exits 0 while a target ran no test and filtered out every one although the command requested no filter. It names the target and exits 65 instead of reporting a green gate that tested nothing (#11661).
- `tm build-lease` now guards a pinned, non-shared `CARGO_TARGET_DIR` the way it guards a slot: it holds a lock in the directory for the build and clears the fingerprints another checkout left there, so one worktree's gate never runs a test binary built from another worktree's sources (#11661).
