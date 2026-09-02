Changed
- The `ticketing` agent's lifecycle section now advances a status label with
  `tm issue transition <n> <state>` instead of a hand-typed
  `gh issue edit --add-label … --remove-label …`. The transition validates the
  edge against the project's `issue-state.yaml` and issues both label flags as
  one `gh issue edit`, so two `status:*` labels on one issue is unreachable
  rather than merely forbidden in prose. A close now names its evidence
  (`--note`), and the hand-typed single-call edit is documented only as the
  fallback for a host without `tm`. Claim comments at dispatch are unchanged —
  the transition's own audit line is not the dated claim record.
