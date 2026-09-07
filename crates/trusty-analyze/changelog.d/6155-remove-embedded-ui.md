Removed
- The admin UI is gone from this crate: the `ui/` Svelte tree, the committed
  `ui/dist/` bundle, `build.rs` and its Vite step, the `build`/`exclude` keys in
  `Cargo.toml`, and the `rust-embed` and `mime_guess` dependencies. The Svelte
  source moved to `crates/trusty-console/ui-analyze`, and the console serves the
  dashboard at `/tools/analyze/` over this daemon's existing `analyze.*` socket
  methods. Nothing had referenced these assets since #6287 deleted this crate's
  embed and its HTTP listener, so no public item changes shape and no version
  bump is owed on that account
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
