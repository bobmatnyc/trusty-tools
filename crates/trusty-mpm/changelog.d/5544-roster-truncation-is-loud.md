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
