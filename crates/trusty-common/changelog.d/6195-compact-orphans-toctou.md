Fixed

- `compact_orphans` no longer deletes a concurrently-upserted vector row as a
  false orphan. It re-checks each candidate's liveness inside the delete
  transaction, so a `VECTORS` row that is live at the moment of deletion is
  never removed — closing a TOCTOU window where a drawer's embedding could be
  dropped and the drawer left permanently unsearchable (#6195).
