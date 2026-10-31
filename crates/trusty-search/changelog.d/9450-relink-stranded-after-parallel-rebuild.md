Fixed

- A graph compaction or precision backfill that rebuilds with several threads now searches every key with its own vector afterwards and re-inserts any survivor the concurrent inserts left unreachable, so a rebuilt index never serves a stranded vector; the count is logged as `re-linked N stranded vectors after parallel rebuild` (#9450).
