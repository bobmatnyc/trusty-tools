Fixed
- `build.rs` no longer watches the `ui/` files #6155 deleted. Cargo treats a missing watched path as changed, so every `cargo build` re-ran the build script and recompiled trusty-search and its dependents. The script now watches `ui-dist/`, so a remirrored bundle still reaches the binary.
