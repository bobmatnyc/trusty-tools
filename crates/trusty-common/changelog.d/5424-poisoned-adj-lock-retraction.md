Fixed

- A KG retraction that committed to redb no longer reads back as one that never
  happened ([#5424](https://github.com/bobmatnyc/trusty-tools/issues/5424)).
  `retract_triple`, `cascade_delete_by_drawer`, `retract` and `assert` commit
  before updating the in-memory adjacency; when that second step failed on a
  poisoned lock they returned a bare `kg adjacency lock poisoned` error, and a
  retry then read `Ok(0)` from storage — indistinguishable from "that fact was
  never here", with the opposite remediation. They now return the new
  `memory_core::store::kg::AdjacencyDesync` error: `CommittedButStale` names
  the operation and the rows storage already made durable, and every later
  mutation on the handle stops with `HandleStale` instead of answering a
  plausible `0`. `KnowledgeGraph::adjacency_desynced()` reports the state, which
  is sticky for the life of the handle — redb stays authoritative and the
  adjacency is rebuilt by reopening the palace.
