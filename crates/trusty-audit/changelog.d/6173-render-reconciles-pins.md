Fixed
- `render` now refuses when the engagement's own `tools/trusty-review` is at a
  different version from the engagement's `trusty-review` pin, naming both. It
  used to render at whatever that copy happened to be and exit 0. `install`,
  `run` and `guided` all reconcile pins; `render` never did, and its default
  renderer IS that engagement-local copy — so bumping a pin and re-rendering
  silently produced the report at the old version, with the only trace a version
  in the report metadata, read after the report had been believed.
- The reconciliation covers only the engagement's copy. `--review-bin` stays the
  escape hatch and is never reconciled, and a `PATH`-resolved renderer keeps the
  disclosure it already carried rather than gaining a refusal. A re-render where
  no `engagement.toml` loads pins nothing and is unaffected.
