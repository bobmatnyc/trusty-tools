Added

- `commits.ai_detection_method` records which signal family produced each
  `is_ai_assisted` verdict — `trailer`, `message`, or `email` — and is NULL for
  a commit no marker claimed (#4418). Consumers can now cut the trailer-matched
  subset, the one they can re-derive from commit messages themselves, out of
  the AI-assisted total instead of reading the total as if it already were that
  subset. The column is nullable and additive: `is_ai_assisted` and `ai_tool`
  keep their existing semantics, and an older `tga` reading the database is
  unaffected. Migration v29 adds the column; `DETECTOR_VERSION` moves to 2, so
  the next `tga collect` repopulates every stored row, and
  `tga backfill ai-detection-commits` writes it too.
- The weekly report and `weekly_activity.csv` gain `ai_trailer_count`,
  `ai_message_count` and `ai_email_count`, which partition `ai_assisted_count`
  by that same signal family (#4418). Appended after the existing columns, so a
  consumer reading by column index is unaffected.
