Changed
- `tm pr open --rung N` now applies the label `rung:N` to the PR, creating it if missing.
- `tm pr queue-check` refuses a PR with no `rung:N` label, with two rung labels, or with a malformed one, and refuses a `rung:5` or `rung:6` PR unless a trusted APPROVE or WARN is pinned to the head. Its code-critic BLOCK check now uses the same reader as `tm pr merge`, so only OWNER, MEMBER and COLLABORATOR verdicts count (owner ruling Dr4).
