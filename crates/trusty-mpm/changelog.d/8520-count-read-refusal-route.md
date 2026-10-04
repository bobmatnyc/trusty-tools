Changed
- pm-guard's refusal of a count-only read of a secret-bearing file (`grep -c`, `grep -q`, `grep -l`), on the host or sent through `ssh` or `aws ssm`, now says why a count is refused and names the supported route (#8520).
