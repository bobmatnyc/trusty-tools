Breaking
- `session_context_catchup` and `tm sessions catchup` no longer return `sessions`, `recent_commits` or `recent_memory` unless called with `full: true` / `--full`. `all_projects` / `--all-projects` and a non-zero `sessions_offset` / `--sessions-offset` without `full: true` / `--full` are now errors (#9883).
