Changed

- The write-quarantine module docs no longer list genuine corruption among the
  conditions that trigger it. Corruption is absorbed before `corpus_open_failed`
  can be set: `open_corpus_db_or_recreate` classifies it as recoverable, moves
  the file aside, and returns a fresh empty corpus. Only lock contention and
  transient I/O reach the quarantine, so its population is transient-dominated —
  which the docs now say, along with the recreate-to-empty gap it does not cover.
