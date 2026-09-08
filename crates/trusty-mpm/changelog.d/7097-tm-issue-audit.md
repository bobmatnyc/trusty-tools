Added

- `tm issue audit <N>` verifies a filed issue against the ticketing standard, printing PASS/FAIL/SKIP per requirement — project, milestone, component label — plus relationships as INFO, and exiting 1 on a violation. A `no-milestone: <reason>` comment satisfies the milestone requirement and prints as SKIP with the reason quoted (#7097).
- `tm issue audit --recent <n>` and `--since <YYYY-MM-DD>` audit a window of OPEN issues as a summary table, exiting 1 if any fail. Pull requests are excluded, since `gh issue list` returns issues only (#7097).
- A `tm doctor` `issue_audit_recent` check sweeps the OPEN issues from the last 7 days, warning with the failing issue numbers and the requirement each missed. Advisory — ticket hygiene never turns doctor red — and UNDETERMINED rather than a pass when `gh` is absent, unauthenticated, or erroring (#7097).
