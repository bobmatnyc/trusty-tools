Changed
- The console reaches trusty-search over its Unix socket instead of loopback
  HTTP (ADR-0032). The service card dials `search.health`, the index-delete and
  batch-prune routes dial `search.index.delete`, and `/api/search/*` — the prefix
  the console-served search dashboard calls — is translated into RPC calls rather
  than reverse-proxied. The two Server-Sent Event streams the dashboard opens,
  `/status/stream` and `/indexes/{id}/reindex/stream`, are bridged from the
  daemon's RPC streams frame for frame, keep-alive comment included. Nothing now
  reads trusty-search's `http_addr` discovery file, so a stale one left by a
  pre-migration daemon can no longer forward the dashboard to whatever holds
  7878 (#6285).
