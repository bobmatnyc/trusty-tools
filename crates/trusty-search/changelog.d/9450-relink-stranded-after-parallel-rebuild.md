Fixed

- A graph compaction or precision backfill now searches every key with its own vector after every rebuild, single-threaded or not, and re-inserts any survivor the build left unreachable, so a rebuilt index never serves a stranded vector; the count is logged as `re-linked N stranded vectors after rebuild` (#9450).
