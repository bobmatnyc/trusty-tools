Added

- The `code-review-standards` Fail-Open Check gains two checks: `cmd | grep -q` under `set -o pipefail` (SIGPIPE makes a found match read as a failure), and a pre-filtered `gh` test stub. The PM workflow section now says the check has seven items (#8974).
