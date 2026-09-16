Fixed

- `tm wait` bounds each poll by the time left in its `--slice`, and no longer starts a poll on the ceiling it has no budget for. `tm wait --for check` runs `gh pr view` through the kill-on-timeout runner, so a stalled read returns a documented `pending`/`timeout`/`error` status instead of carrying the invocation past an enclosing timeout into a SIGKILL (exit 137) ([#7956](https://github.com/bobmatnyc/trusty-tools/issues/7956))
