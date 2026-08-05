Added

- `store::room_plan::plan_rooms` — the read-only plan of what a room backfill
  would write (ADR-0027 T10), behind the new `trusty-memory rooms backfill
  --dry-run`. `backfill_rooms` now executes exactly this plan rather than
  re-deriving it, so the audit an operator approves and the write that follows
  cannot disagree. Planning opens read transactions only and is proven not to
  change one byte of `ROOMS`, `ROOM_KEYS`, or `DRAWERS`.
