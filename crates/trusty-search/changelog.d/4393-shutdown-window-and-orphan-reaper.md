Fixed

- The shutdown flush can now actually finish. Its per-index budget floors at
  30 s and ceilings at 20 min, while every window that terminates the daemon
  granted 3–5 s: launchd's `ExitTimeOut` default (measured 5 s on macOS, not
  the documented "system-defined" anything), `trusty-search stop`, and the
  orphan reaper. A flush with real work to do was SIGKILLed mid-sweep on every
  path, losing HNSW vectors committed since the last checkpoint. Two changes:
  every generated LaunchAgent plist now declares `ExitTimeOut`, and `stop` and
  the reaper wait the same window. **An already-installed LaunchAgent keeps
  launchd's 5 s default until its plist is regenerated** — re-run the
  installer's service setup to pick it up (#4393)
- A per-index flush deadline can no longer outlive the process that granted it.
  Deadlines are now minted by a `ShutdownBudget` counting down from the instant
  SIGTERM landed, so a sweep that runs out of window stops cleanly at an index
  boundary — logging how many indexes kept their last incremental checkpoint —
  instead of being cut off partway through a write (#4393)
- `trusty-search start` no longer SIGKILLs healthy daemons that merely share
  its executable name. Its orphan reaper matched on process name plus `start`
  in argv, so any lock-visibility asymmetry — a second instance under
  `--data-dir`/`TRUSTY_DATA_DIR`, a daemon restarted without the override it
  started with, a deleted lockfile — turned a routine `start` into the
  destruction of a live production daemon, with 3 s to shut down. The reaper
  now reaps only processes it has positively identified as sharing its own data
  directory, read from the candidate's own `--data-dir` argument or
  `TRUSTY_DATA_DIR`; a process whose argv or environment cannot be read is
  spared and reported, never killed. `trusty-search stop` keeps its explicit
  stop-everything contract (#4395)
- Two daemons indexing the same repository can no longer splice each other's
  HNSW snapshot. Both staged through the same `hnsw.usearch.tmp` before
  renaming, and colocated indexes keep that file in the project root — outside
  every data directory, so even fully data-dir-isolated daemons collided there.
  The staging name is now scoped to the writing process (#4395)
