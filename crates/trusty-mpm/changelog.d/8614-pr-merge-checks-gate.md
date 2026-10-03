Fixed

- `tm pr merge` refuses (exit 1) while any check on the PR head has failed, whether or not branch protection requires it, and names each failing check (#8614). With `--auto` it also refuses while a check that auto-merge would not wait for is still running: GitHub's auto-merge waits only on required checks, so such a check could fail after arming and the PR merged anyway. The required checks are read from the base branch's `.protection` on `repos/{owner}/{repo}/branches/{branch}`, which answers for an unprotected branch too.
