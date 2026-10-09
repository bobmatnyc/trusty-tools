Fixed
- A memory search no longer deadlocks against a concurrent write on the same
  palace. The exact-scan search re-acquired a read lock inside `hnsw_rs`'s point
  iterator; a queued insert blocked that second acquire while waiting on the
  first, which wedged the palace until a daemon restart. The scan now takes one
  short read per graph layer (#9487).
