Fixed

- `build.rs` now resolves `cargo:rerun-if-changed` for `HEAD`/`index` from the
  real git dir (walking up from `CARGO_MANIFEST_DIR`, following a worktree's
  `gitdir:` pointer where applicable) instead of two crate-relative paths that
  never existed — those forced a full `trusty-agents` rebuild on every `cargo
  build`/`cargo test` invocation, costing 2-10 minutes each time (#8787)
