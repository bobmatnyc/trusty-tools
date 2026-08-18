Fixed

- **The `make`-unavailable fallback in `build.rs` now re-stamps
  `ui-dist/ui-source-hash.txt`.** `make release-prep` stamps via `sync-ui`, but
  the direct-pnpm fallback mirrored `ui/dist` into `ui-dist/` and stopped
  there, leaving the stamp claiming a bundle that had since been rebuilt
  ([#5936](https://github.com/bobmatnyc/trusty-tools/issues/5936))
