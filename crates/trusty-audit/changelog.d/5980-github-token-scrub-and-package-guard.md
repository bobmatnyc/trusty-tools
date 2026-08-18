Fixed

- The scrubber that keeps a credential out of a spawned child's log, and the
  guard that refuses to package a file carrying one, both now cover the
  `gh`-derived GitHub token — previously only `EngagementConfig`'s own
  secrets were checked, so a rejected token echoed back by a child could
  reach the log or the deliverable unredacted (#5980).
- Packaging now refuses when the active `gh` credential differs from the one
  the sweep collected under, instead of silently scanning outbound files with
  the wrong token. Packaging can run as a separate process from the sweep —
  possibly under a different `gh` account after the operator re-authenticated
  — and the previous outbound scan re-resolved the credential fresh each
  time, so a token a child echoed at sweep time could ship in the deliverable
  unredacted because the newly-resolved token never matched it. A truncated,
  non-reversible fingerprint of the sweep's credential is now recorded in the
  checkpoint and compared at packaging time; a checkpoint written before this
  fingerprint existed still packages, with the uncertainty stated rather than
  silently assumed safe (#5980).
