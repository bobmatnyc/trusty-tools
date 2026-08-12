Fixed

- A delegation roster that lost agents to a failed read now says so. An agent
  file that exists but cannot be read used to be dropped by a bare `continue`
  with no log, and a tier that failed to enumerate returned an empty list
  indistinguishable from an absent tier — so a PM was handed a short roster and
  read it as the complete set. Both losses are now recorded in a `RosterScan`,
  logged at `error!`, and rendered as a `ROSTER INCOMPLETE` banner at the top of
  the composed `## Delegation Authority` section naming every unreadable path.
  A roster that is empty *because* reads failed no longer degrades silently to
  the bundled asset. Composition stays fail-open — a bad directory must not
  block a launch — but it can no longer claim a roster it does not have
  (#5544).
- Every surface that publishes the roster COUNT now says when the count is a
  floor. `tm session start` prints `at least N agents in delegation authority —
  ROSTER INCOMPLETE: …` and names each unreadable path on stderr; `tm doctor`'s
  `agents` check downgrades `Ok` to `Warn` and reports `at least N delegatable`
  with the lost paths. Both took their number from the non-reporting
  `resolve_roster`, so a failed read lowered it silently — including in
  `tm doctor`, which is the tool the new banner tells the operator to run
  (#5544).
