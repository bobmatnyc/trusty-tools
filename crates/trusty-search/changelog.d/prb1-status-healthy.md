Added
- `trusty-search status --json` carries `healthy` (`true` when the daemon answers) and a separate `unhealthy_indexes` list of held, degraded, failed or unreadable indexes. The not-running body is `{"daemon":"not_running","healthy":false}` and still exits non-zero.
