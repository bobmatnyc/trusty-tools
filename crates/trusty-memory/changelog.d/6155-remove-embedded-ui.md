Removed
- The embedded admin UI is gone from this crate: `src/ui_assets.rs` (the
  `rust_embed` bundle and its `asset` lookup), `build.rs` and its Vite step, the
  `rust-embed` and `mime_guess` dependencies, and the committed `ui/dist/`
  bundle. The Svelte source moved to `crates/trusty-console/ui-memory`, and the
  console serves the dashboard at `/tools/memory/` over the daemon's existing
  `memory.*` socket methods. Nothing had served these assets since #6286
  deleted this crate's HTTP listener — the removal takes the public
  `ui_assets::WebAssets` and `ui_assets::asset` items with it, which is a
  breaking change and owes a MINOR bump at release under Cargo's 0.x rule
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
