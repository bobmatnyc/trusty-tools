Removed
- The `memory` proxy row. `/api/memory/*` resolved a base URL from an `http_addr` file trusty-memory no longer writes but which is still on disk from before the migration, so the row could only forward to whatever now holds 7070. Deleted for the same reason the `analyze` row was (#6287), not kept inert like `review`'s
- trusty-memory's `7070` row in the known-sibling port-collision table. It binds no TCP port since ADR-0032, so reserving 7070 against it would only forbid a future daemon a free port
