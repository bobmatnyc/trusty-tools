Fixed

- `ticketing` agent no longer applies `trusty-mpm` as an "umbrella" crate
  label on every issue it files. The instruction that caused it — apply
  `trusty-mpm` to "anything surfaced through tm-orchestrated dogfooding" —
  fired on every ticket the agent created, since the agent always runs
  inside a tm-orchestrated session. Replaced with a positive rule: a crate
  label names the crate whose code the defect lives in, read from the file
  path the finding cites; when no crate label fits, apply none. `trusty-mpm`
  is now a crate label like any other, applied only when the finding's own
  file path is under `crates/trusty-mpm/` (#5679).
