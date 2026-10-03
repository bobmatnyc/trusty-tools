Added

- `tm pr merge --allow-failing <check>` merges over a failing or still-running check that the PM has waived (#8614). It is repeatable, matches the check name exactly, never waives a check that branch protection or a repository ruleset requires, and records each waived check in the output and at the head of the squash commit body.
- `tm pr merge --allow-no-checks` merges a PR on which no check has registered, for a repository with no CI (#8614). Without it, such a PR is refused unless the base branch requires a check.
