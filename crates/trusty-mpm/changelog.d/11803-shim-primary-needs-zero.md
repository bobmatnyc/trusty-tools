Fixed

- The pinned gh shim names the primary GraphQL rate limit, with its "resets at" time, only when its `gh api rate_limit` lookup reads `remaining` as 0. When the lookup fails or `remaining` is unreadable, the refusal line names a GraphQL limit of unknown kind and gives no reset, so a secondary limit is no longer reported as the primary one with the hourly window's reset. `tm wait` and the daemon's PR watch back off briefly on that line (#11803).
