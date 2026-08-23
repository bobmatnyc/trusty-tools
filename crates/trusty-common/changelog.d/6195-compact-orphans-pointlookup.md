Fixed

- `compact_orphans` now re-confirms each candidate's liveness with a
  per-candidate point lookup rather than a full `VECTOR_KEYS` scan inside the
  delete transaction, so the write-lock hold stays O(candidates) instead of
  O(live-count) — it no longer blocks concurrent upserts for a full live-table
  scan on large palaces while still closing the #6195 TOCTOU.
