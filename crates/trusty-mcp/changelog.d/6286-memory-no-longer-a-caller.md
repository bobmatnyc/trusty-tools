Changed
- `daemon_bridge`'s docs no longer name trusty-memory as a caller. Its stdio bridge polls its own Unix socket in a local readiness loop since #6286 — the same disposition trusty-analyze took — so trusty-search is the module's one remaining consumer. No code change
