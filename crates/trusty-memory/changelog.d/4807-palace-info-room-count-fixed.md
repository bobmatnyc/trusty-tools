Fixed

- `PalaceInfo.wing_count` stopped reporting a room count (closes
  [#4811](https://github.com/bobmatnyc/trusty-tools/issues/4811), ADR-0027 T8).
  Since the day it shipped, the field rendered as "N wings" in the palace UI and
  the TUI health panel while actually carrying the number of distinct rooms. A
  truthful `room_count` is now reported alongside it, read from the `ROOMS`
  registry so it agrees with `room_list`, and both readers were migrated to it.
  `wing_count` is retained as a deprecated wire field reporting `1` — there is
  exactly one wing (`DEFAULT_WING_ID`) in every palace until ADR-0027 T9 adds the
  Wing entity — and `0` when the palace is not resident, matching the
  unknown-vs-empty contract of every other count on the row.
