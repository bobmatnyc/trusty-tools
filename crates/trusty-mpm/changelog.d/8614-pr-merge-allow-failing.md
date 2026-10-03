Added

- `tm pr merge --allow-failing <check>` merges over a failing check, or with `--auto` a running one, that the PM has waived (#8614). It is repeatable, matches the check name exactly, never waives a required check, and records each waived check in the output and at the head of the squash commit body.
