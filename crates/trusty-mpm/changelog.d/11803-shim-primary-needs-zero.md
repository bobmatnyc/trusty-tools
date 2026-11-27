Fixed

- The pinned gh shim names the primary GraphQL rate limit, with its "resets at" time, only when its `gh api rate_limit` lookup reads `remaining` as 0. When the lookup fails or `remaining` is unreadable, the refusal line names a GraphQL limit of unknown kind and gives no reset, so a secondary limit is no longer reported as the primary one with the hourly window's reset. `tm wait` and the daemon's PR watch back off briefly on that line (#11803).
- When gh printed a `retry-after` value (on stdout, as `gh api -i` headers do, or on stderr), the shim's secondary and unknown-kind refusal lines state it: "GitHub asked to retry after N seconds (retry-after)". The primary line with no readable reset now says "gh api rate_limit gave no reset" (#11803).
