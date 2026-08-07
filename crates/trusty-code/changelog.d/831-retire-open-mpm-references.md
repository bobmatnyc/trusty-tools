Fixed

- Removed a stale `.open-mpm/workflows/<name>.toml` fallback mention from the
  (still-unimplemented) `run-workflow` help text and corrected
  `build_info.rs`'s doc-comment example path, which is caller-supplied and
  was never actually `.open-mpm/state/build.json`.
