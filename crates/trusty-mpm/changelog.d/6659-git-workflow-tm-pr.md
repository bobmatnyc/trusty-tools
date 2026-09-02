Changed

- `git-workflow` skill — the "trusty-tools Deterministic Tools" table gained
  rows for `tm pr open`, `tm pr queue-check`, `scripts/required-checks.sh`,
  and `scripts/is-branch-caused.sh`, alongside the existing rows.
- `tm-workflow` skill — "Shipped Defaults on the PR" now notes that
  `tm pr open` attaches the assignee, both labels, and the attribution
  footer itself and refuses to call `gh` without them; "Merge-Queue
  Ownership — the Procedure" now notes that `tm pr queue-check` runs the
  whole stop-condition table and exits nonzero on the first stop it finds
  (Refs #6659).
