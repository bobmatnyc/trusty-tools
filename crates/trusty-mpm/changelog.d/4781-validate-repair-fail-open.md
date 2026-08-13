Fixed

- `validate_and_repair` no longer reports `repaired: true` for a repair that
  failed (closes [#4781](https://github.com/bobmatnyc/trusty-tools/issues/4781)).
  The flag was set unconditionally on the repair path, so a workspace whose
  repair was refused outright — `prepare_session_with_repo_url` returning the
  fatal `PrepError::Instructions` (#4752) — still reported itself repaired next
  to a populated `repair_error`. It is now derived from the re-validation:
  `true` only when the pipeline returned no error AND the post-repair report has
  no gaps.
- `tm validate --repair` prints the attempt's diagnostics again. It gated them
  on `repaired`, which under the new meaning would have silenced the error
  message on exactly the runs that produce one; it now gates on whether the
  pre-repair report had gaps, and says so when the repair did not restore the
  workspace.
