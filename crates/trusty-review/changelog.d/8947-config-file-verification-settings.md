Fixed

- `trusty-review run --config <file>` now applies the whole file:
  `[verification]`, `[models.verifier]`, `[models.reviewer]`, `[review]`,
  `[voice]`, `[coverage]` and `[context]`. `run` rebuilt its config with the
  model overrides from no file at all, so only the environment overrides took
  effect. `calibrate` had the same defect and is fixed the same way.
