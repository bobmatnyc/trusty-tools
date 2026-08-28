Changed

- The `console_metrics` payload adds `counted_palace_count`, `total_rooms`, and
  per-palace `room_count`, `stats_source` (`cache` / `disk` / `unavailable`) and
  `stats_error`; `metrics_schema_version` is now 3. `cached_palace_count` and
  the per-palace `cached` flag keep their meaning — how many palaces were
  resident — so an older console still reads the report (#6372)
