Fixed

- `DELETE /indexes/:id` now clears the index's cold-store records. Deleting a
  cold-parked or restore-failed index left them behind, so `/status`,
  `/chunks`, and `grep` answered 503 forever for an id that no longer existed —
  inverting #5057's rule that 404 means "absent from every store". A
  cold-parked-only index is also removed from `indexes.toml` now, instead of
  being resurrected by the next warm boot.
