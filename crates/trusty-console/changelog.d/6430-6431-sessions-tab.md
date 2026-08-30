Added
- Sessions tab: every session row shows its last-used date (or "never"), and a
  sort control orders every group by it; rows with no recorded activity always
  sort last (#6430).
- Sessions tab: the unknown bucket — records whose lifecycle state is missing or
  unrecognised — supports multi-select and a record-only bulk delete, behind an
  explicit confirmation that lists every session it will delete, with its
  reported status. Deletion never removes a worktree or workspace directory, a
  session that is still running is refused rather than deleted, and a failed
  deletion is reported failed rather than counted as a success (#6431).
- `POST /api/console/sessions/bulk-delete`, backed by trusty-mpm's
  `session_delete_records` MCP tool (#6431).
