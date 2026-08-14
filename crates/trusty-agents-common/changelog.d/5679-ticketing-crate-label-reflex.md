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
- Closed a follow-on loophole in that same rule: a live run correctly found
  no crate applied to a `.github/workflows/ci.yml` defect, then attached
  `trusty-mpm` anyway as a self-invented "provenance" label recording which
  session found it. The rule now states directly that no such second label
  axis exists — the origin of a finding is never a labeling input under any
  name, and "no crate label fits" is final, not a fallback trigger for
  `trusty-mpm` (#5679).
