Fixed

- `TGA_RATE_LIMIT_SLEEP_BUDGET_SECS` is now discoverable (#6565). The knob
  shipped with the per-run budget but appeared in no `tga --help`, no
  `tga collect --help`, and no documentation — `RATE_LIMIT_SLEEP_BUDGET_ENV` was
  `pub(crate)` and never surfaced, so an operator whose sweep needed a larger
  allowance had no way to learn one existed. `tga collect --help` now carries an
  `ENVIRONMENT:` section naming the variable, its 120 s default, and the rule
  that zero, empty, and unparseable values fall back to that default; the same
  detail is in the crate README's configuration section and in the workspace
  environment-variable table. A test renders the subcommand's help and asserts
  the constant's own value appears in it, so renaming the constant without
  updating the help fails rather than silently un-documenting the variable.
