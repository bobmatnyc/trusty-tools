Added

- A `log_drain.sources[]` entry can carry its own `destination`, so one daemon
  drains different projects to different object stores — and to different AWS
  accounts, using the `?profile=` / `?role_arn=` support added in
  `trusty-common` (#6657). A source that names none inherits the section's
  `destination` as before. Sources are grouped by the destination they resolve
  to and the scheduler runs one pass per group, each with its own connection and
  its own manifest; the manifest cache was already keyed by destination (#6548),
  so nothing about that changed. `log_drain.destination` is now required only
  when some source still needs it.
- The `log_drain` doctor row lists every configured destination and quotes each
  one's own last-run outcome, instead of reporting a single verdict for all of
  them. `status.json` gained a `destinations` array; a file written before this
  change still decodes and the row falls back to its single detail line.
