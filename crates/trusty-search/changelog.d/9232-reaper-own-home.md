Fixed
- `trusty-search start` under another HOME (a sandbox) without `--data-dir` no longer signals the live `com.trusty.search` daemon. The orphan reaper resolves a candidate's default data dir from the candidate's own `HOME`, not the reaper's, and spares a candidate whose `HOME` cannot be read (#9232).
- The reaper re-proves each orphan from a fresh process scan before SIGTERM and again before SIGKILL: same pid, same start time, still on this data dir. A reused or changed pid is not signalled and the reason is logged (#9232).
- `trusty-search service restart` applies the same per-candidate `HOME` resolution when it picks the daemons to terminate (#9232).
