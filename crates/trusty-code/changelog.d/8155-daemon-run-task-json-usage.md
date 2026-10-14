Fixed
- `tcode run-task --json` on the default daemon path now carries `transcript`, `turns`, `usage` and `cost_usd`, read from the run's own daemon before it exits, so a caller gets per-run cost without `--legacy-in-process`. The human output gains a one-line usage footer, and the legacy report gains the same `turns` count (#8155).
