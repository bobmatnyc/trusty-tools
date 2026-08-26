Changed
- The monitor's activity log no longer shows an event up to a tick late, and no longer misses one evicted from the activity log between two ticks. A daemon that predates `memory.activity_stream` still works: the failed open is reported and the poll carries the log
