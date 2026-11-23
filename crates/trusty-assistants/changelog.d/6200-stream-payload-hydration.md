Changed

- `TrustyBackedMemoryStore::new` hydrates its in-memory sidecar by streaming
  rows from the payload store one at a time (`try_for_each_row`) instead of
  `load_all(None)` (#6200). A large palace no longer buffers the entire payload
  table into an owned `Vec` on top of the sidecar map it is building.
