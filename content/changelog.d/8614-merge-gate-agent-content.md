Fixed
- `version-control` and `tm-workflow` describe the `tm pr merge` checks gate: a failed check, no registered check, or a running check it would not wait for refuses the merge. A branch-caused failure is never waived, and `--allow-failing <check>` waives only a non-required check the brief names (#8614).
- `version-control`, `tm-workflow` and `git-workflow` no longer let a non-required pending check through, and no longer suggest `--admin` or raw `gh pr merge` around a refusal (#8614).
- The required-checks fallback read uses the branch endpoint's `.protection` field, because `/protection` answers 404 on an unprotected branch (#8614).
- `security` states the range's `git diff --name-only | wc -l` count and the count it scanned in a credential-scan report. A mismatch makes the report INCOMPLETE, never PASS (#8504).
- `version-control` reads `gh repo view --json autoMergeAllowed` before planning a merge and reports it up front. `tm-workflow` states that auto-merge is never assumed, and that a `false` answer means a direct merge once checks settle (#8640).
