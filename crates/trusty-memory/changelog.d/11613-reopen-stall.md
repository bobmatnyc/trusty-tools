Fixed
- The idle-evict sweep keeps a palace whose last open took 1 s or more, so the first call after an idle period no longer re-opens it past the client's 5 s exchange and fails with a broken pipe (#11613).
- Every per-palace tool (`memory_remember`, `memory_note`, `memory_recall`, `memory_list`, `memory_forget`, the `kg_*` and `task_*` tools) now waits at most 3 s for a palace that is not resident, then answers `-32007` (`palace_opening`) while the open continues in the background. The handler has not run, so a refused write is safe to re-send (#11613).
