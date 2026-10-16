Fixed
- Hook-fed worktree ingest: a stop event now always reads the project registry, so an agent-isolation tree that failed to record while the stop waited is retried against real checkouts instead of being dropped as unregistered, and the `worktree_registry` doctor alarm stays up until it is recorded (#8994).
