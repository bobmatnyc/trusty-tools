Fixed

- `memory_forget` now deletes the drawer's BM25 document, so a forgotten drawer
  stops being findable on the lexical lane (#5053)
  - forget removed the drawer from redb and the vector store and never called
    `Bm25Client::delete`, so the drawer's full text stayed in the palace's BM25
    corpus — matching lexical queries, contributing to RRF fusion, and staying
    resident in the daemon's memory and its on-disk snapshot
  - the delete is awaited rather than queued behind the bounded index channel:
    a dropped index request is repaired by the backfill, but the backfill only
    adds, so nothing anywhere would re-attempt a dropped delete
  - when the lane is armed and its daemon cannot be reached, `memory_forget`
    now returns an error naming the drawer instead of reporting `deleted` —
    a caller must be able to tell "deleted everywhere" from "deleted where we
    could look". With the lane off (`TRUSTY_BM25_DAEMON` unset) nothing changes
  - the HTTP `DELETE /palaces/{id}/drawers/{drawer_id}` path deletes the same
    document, since the backfill indexes a drawer however it was written
