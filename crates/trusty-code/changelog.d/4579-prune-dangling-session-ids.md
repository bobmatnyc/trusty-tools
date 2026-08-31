Fixed

- **Boot reconciliation prunes dangling workstream `session_ids` (issue
  #4579).** `WorkstreamStore` persists each workstream's `session_ids`, but
  `SessionRegistry` is in-memory only, so after a daemon restart every
  persisted id referenced a session that no longer exists and was never cleaned
  up. `reconcile_on_boot` now takes the live session-id set and drops, from
  every record, any id absent from it — a live id is always kept, and no
  workstream record is ever removed (AC-6.1 unchanged). At the real boot site
  the registry is empty, so all stale references are pruned; the change is
  persisted only when something actually changed, and never touches a record's
  `updated_at`.
