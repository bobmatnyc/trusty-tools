Fixed
- #6556: the `SubagentStop` hook POST is retried with a bounded backoff inside the hook's registered timeout, every attempt and the verdict are logged to stderr, and a stop the daemon never accepts is parked on disk for the daemon's reap loop to replay — instead of being lost and leaving the delegation `Running` for six hours.
- #7487: a dispatch the daemon admits revives the `Cancelled` record its own earlier deny wrote for the same `tool_use_id`, so a running agent is never described as ended with its worktree reclaimable under it.
