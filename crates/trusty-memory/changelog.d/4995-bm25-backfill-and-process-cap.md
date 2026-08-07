Added

- `bm25_backfill` — a lossless feeder that indexes a palace's existing drawers.
  The live write path (`bm25_index_enqueue`) holds a 256-slot channel written
  with `try_send` and drops on full, so a backfill routed through it would have
  dropped roughly 80% of the largest palace's 1311 drawers and left it answering
  lexical queries from a fifth of its content. The feeder awaits each document's
  ack instead, so nothing is ever offered to a full queue. Idempotent, bounded
  by a per-op and a per-palace deadline, and it reads coverage back from the
  daemon rather than trusting its own submission count. Runs as a serial startup
  sweep over palaces that have drawers; `TRUSTY_BM25_NO_BACKFILL=1` defers it.
