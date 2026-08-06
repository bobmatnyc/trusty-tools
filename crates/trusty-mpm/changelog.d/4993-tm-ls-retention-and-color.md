Changed

- `tm ls` now evicts session records that have sat in a terminal state
  (`decommissioned`/`deleted`) for more than 7 days, and releases the `NUM`
  slot each evicted record held. Numbers stopped tracking the visible fleet
  because nothing ever left the store — 76 of 107 slots on one machine were
  held by rows the default view hides, which is why `NUM 107` appeared at the
  bottom of a 31-row listing. Retention runs on the daemon's existing GC tick.
  A record whose workspace directory still exists on disk is never evicted.
  Records written before this change carry no terminal timestamp; they are
  backfilled from their last evidence of life rather than from the current
  time, so an already-stale backlog is cleared on the next sweep instead of
  seven days after upgrading.
- `tm ls` colors the `NUM` and `NAME` columns in two distinct hues (magenta and
  cyan). Deliberately not one hue: a name's trailing serial is per-project and
  reuses gaps, while `NUM` is a global slot, so any match between them is
  coincidence. Color is gated on stdout being a TTY and honours `NO_COLOR`;
  piped and redirected output is byte-identical to before.
