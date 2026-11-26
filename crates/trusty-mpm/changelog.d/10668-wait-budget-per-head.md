Fixed

- `tm wait --for check` keys its `--timeout` budget to the PR and its head SHA. A re-run on the same head keeps the budget; the first poll that sees a new push starts a fresh one. Before, a wait on an earlier head carried its start time into the next CI run and reported `status=timeout` minutes into it (#10668).
