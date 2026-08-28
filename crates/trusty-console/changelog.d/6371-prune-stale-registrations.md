Added
- The Search tab lists trusty-search registrations whose root directory is gone
  and removes the ones an operator confirms, in one batch
  (`POST /api/console/search/prune-indexes`). The candidate list is the daemon's
  own census; a root the daemon could not check is listed and never removed. The
  batch answers one row per id, so a prune where three succeeded and one was
  refused reads as exactly that rather than as "cleaned" (#6371).
