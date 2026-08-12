Fixed

- `claude_config::write_json_atomic` no longer stages through the fixed
  `<path>.tmp`. Two concurrent writers — `tm launch` and the daemon are the
  real pair, and no in-process mutex can cover them — both truncated and
  filled that one file, so whichever renamed first published whatever bytes
  happened to be in it: the other writer's payload, or half of it. A reader
  watching the target during an 8-writer storm observed 128 spliced snapshots
  before the fix and none after. Staging is now `<path>.tmp.<pid>.<seq>`, a
  name no other live writer can hold (#4077).
- The backup carried the same defect: two `fs::copy` calls into one
  `<path>.bak` interleaved into a torn backup, corrupting the recovery artifact
  at the moment it is needed (1188 spliced snapshots observed pre-fix). It is
  now staged and published by rename like the target.
- A write or rename that fails removes its own staging file, so a failed call
  leaves the target byte-for-byte as it was and drops no litter.
