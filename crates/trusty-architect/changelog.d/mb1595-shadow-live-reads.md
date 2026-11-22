Fixed
- `trusty-architect-sched parity` leaves wakes no triage rule decided (the prototype's Haiku path) out of the verdict and both match percentages, reading the live side from `triage.jsonl`, and prints `haiku-path wakes excluded: N`.
- The shadow reads `events.jsonl` from the live inbox too, and its first plan seeds `pm_offset` and `events_offset` at the live files' current end, so the first read is no catch-up burst; every offset stays in the scratch state.
- A PM-message queue that exists but cannot be read keeps its offset instead of resetting it to 0 and replaying the queue later.
- The first shadow plan refuses to seed its live-queue offsets when a live queue cannot be stat'ed as a file or the dry-run state cannot be read or parsed: it returns an error and writes no marker and no state. Only a missing file reads as empty (D64).
