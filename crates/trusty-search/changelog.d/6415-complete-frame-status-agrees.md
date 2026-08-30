Fixed

- A reindex halted by the background memory poller reported `"status":
  "complete"` and `"memory_limit_hit": false` on its terminal SSE frame, while
  the daemon recorded the same run as `AbortedMemory`. The frame derived those
  two fields from the batch loop's own `mem_limit_hit` flag, and the enum status
  derived from `mem_limit_hit || mem_abort` — the poller sets `mem_abort` on its
  own tick and the producer then halts at the next batch boundary, so a run can
  end with `mem_abort` set and `mem_limit_hit` never set. A consumer reading the
  wire string, which is what `trusty_common::monitor::search_client` does, saw a
  memory-aborted reindex as a success and would retry straight into the same
  ceiling. Both fields now come from the terminal status the frame already
  carries, so the payload and the enum cannot disagree. `RunTotals::mem_limit_hit`
  is gone with them. Refs #6415, #6386.
