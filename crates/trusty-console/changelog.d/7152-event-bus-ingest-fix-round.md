Fixed

- The console event-bus ingest listener (#6848) no longer buffers an unbounded amount of memory for a peer that streams a line with no newline — the per-line read cap is now applied before each read (a fresh budget re-taken per line) rather than checked only after an unbounded read returns. It also binds through `bind_singleton_hardened`, so a stale socket file left by an unclean shutdown is reclaimed instead of wedging every future bind, adds a 60 s per-connection idle timeout, and bounds concurrent connections at 256 via a semaphore.
