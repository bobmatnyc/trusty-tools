Removed

- Removed the unused `trusty-mpm.daemonUrl` `localStorage` override in `apiBase()` (closes #3315): nothing in the codebase ever wrote that key, and even if it were set, the CSP `connect-src` (below) would silently block a request to any host other than `DEFAULT_DAEMON_URL` — it was dead code that could never work as written. `apiBase()` now always resolves to `DEFAULT_DAEMON_URL`.
