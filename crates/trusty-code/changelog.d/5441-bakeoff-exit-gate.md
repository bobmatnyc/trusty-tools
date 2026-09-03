Added

- **`tcode bakeoff-gate` — the L1-L3 bake-off milestone exit gate (#5441).** A
  Trusty Code milestone is supposed to close only after a real coding-harness
  bake-off completes levels 1-3 with no unexplained regression against the
  previous accepted baseline. The first independent qualification attempt ran
  all three levels and passed 24/24 verifier checks, and was still disqualified:
  the retained metadata named no candidate commit, no binary hash, no
  runner/challenge revision and no instruction/agent/skill digests, the runner
  checkout was dirty, and L3's +27% wall clock / +50% turns had no recorded
  disposition — and nothing mechanically rejected any of it. The new
  `trusty_code::bakeoff` module declares the retained-evidence schema in the
  crate under test and judges a bundle offline: it rejects incomplete L1-L3
  coverage, missing or empty artifacts, missing provenance (the literal
  `"unknown"` included), mock-only evidence, a dirty runner or candidate
  checkout, and results produced by a different `tcode` build — cross-checking
  each level's `metadata.json` against its own `tcode_report.json` so the
  metadata is evidence rather than an assertion. Against a baseline it blocks
  verifier pass-rate and terminal-status regressions outright, and requires a
  written `dispositions.json` entry for any cost/token/turn/duration change
  beyond the tolerance (20% by default). Exits 0 to close, 1 when the gate
  refuses, 2 when it could not reach a verdict at all. Bundle layout, metadata
  schema and a worked invocation: `docs/reference/bakeoff-exit-gate.md`.
