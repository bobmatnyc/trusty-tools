Fixed
- The embedded `git-workflow` skill reference treats any failed check, or a running check the brief does not waive, as a merge block, and reads required checks from the branch endpoint's `.protection` field (#8614).
- The same reference says a passing `tm pr queue-check` clears the queue checks only; `tm pr merge` still applies the checks gate (#8614).
