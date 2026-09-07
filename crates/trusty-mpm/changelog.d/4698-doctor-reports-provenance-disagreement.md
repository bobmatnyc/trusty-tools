Added

- `tm doctor`'s `asset_tier` probe reports an agent file whose `provenance:`
  contradicts the deployed-agent manifest, naming the file and both records. It
  scans the CANONICAL deploy directory for this, which the probe's shadowing
  scan deliberately skips — that directory is where a hand-edited deployed agent
  lives, so a check that could not look there would miss the case it exists for.
  A disagreement alone is `Warn`, not `Ok`: the manifest still wins for
  ownership so agent resolution is unaffected, but the file was changed by
  something other than a deploy and an operator who is never told has no way to
  find out. It never demotes a shadowing `Fail`, which stays the more actionable
  finding (#4698).
