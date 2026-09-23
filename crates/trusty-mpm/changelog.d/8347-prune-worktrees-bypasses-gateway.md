Fixed
- `tm session prune-worktrees` sends its request straight to the daemon when `tm` resolved the trusty-console gateway, so the URL no longer reads `/api/mpm/api/v1/…` and a multi-minute `--merged-prs` survey is no longer cut to a 502 by the gateway's 30-second proxy timeout (#8347).
