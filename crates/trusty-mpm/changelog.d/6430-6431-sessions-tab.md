Added
- `session_delete_records` MCP tool: record-only bulk deletion over an explicit
  session-id list. Each id routes to whichever registry owns it — the managed
  store's `delete_record`, or the legacy in-memory registry — and the call never
  removes a worktree, a workspace directory, or any other filesystem state, and
  never kills a tmux host. Fail-closed: a session that is still running is
  refused in both registries (there is no `force`), a liveness probe that cannot
  reach a verdict refuses too, and a malformed, unknown, or refused id is
  reported as one failed row rather than counted as a deletion. A deleted legacy
  entry that shares its tmux name with a live managed record reports that record
  as `managed_sibling` and leaves it untouched (#6431).
