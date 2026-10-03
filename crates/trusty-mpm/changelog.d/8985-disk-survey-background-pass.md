Fixed

- `disk_survey` no longer answers a fleet too large for its 55-second clamp
  with the same partial pass on every call. A pass that runs out of budget
  starts one unbudgeted background pass; later calls get the last complete
  pass at once, with per-worktree byte counts. Every response carries
  `freshness` (`live`, `cached` or `partial`), `age_seconds` and
  `background_pass`, and a budgeted pass waits for the shared size index no
  longer than the time it has left.
- The worktree disk-usage gate reads the same figure `df` prints for the
  mount, via `statvfs`, instead of `sysinfo`'s "available for important
  usage", which put a mount at 93% while `df` read 74%. A refusal now states
  the used and available bytes it measured (#8528).
- `tm pr cleanup` states how each kept worktree's tip relates to the merged
  head: it is the head, it is an ancestor (nothing unpushed), or it carries N
  commits the merge did not, which gets a separate WARNING line. An
  unreadable relation is stated as unknown (#8603).
