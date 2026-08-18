Fixed

- **A `cargo build` no longer deletes the committed UI bundle's
  `ui-source-hash.txt`.** `ui/vite.config.js` sets `build.emptyOutDir`, so
  `pnpm run build` clears the bundle directory and takes the stamp with it;
  only `scripts/stamp-ui-bundle.sh` writes it back and nothing chained the two.
  `build.rs` now re-stamps whenever a build actually ran. The polarity was the
  reverse of the obvious guess — a host with no pnpm bails before the build and
  deletes nothing, so only machines with a JS toolchain were affected
  ([#5936](https://github.com/bobmatnyc/trusty-tools/issues/5936))
