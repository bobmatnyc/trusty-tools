Fixed
- Architect dashboard: requests queued behind a failed `/api/fleet` or cold `/api/harness` build share its error for the cache TTL, so N requests behind a hung store writer wait one lock wait, not N.
