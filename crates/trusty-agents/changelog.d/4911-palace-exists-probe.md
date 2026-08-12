Fixed

- `TrustyBackedMemoryStore` now probes for an existing palace through `PalaceStore::metadata_present` instead of `palace.json.exists()`. A stat the process was DENIED read as "no palace here", which sent the caller into `create_palace` and rewrote the metadata of a palace that was present all along — the #5549 / ADR-0045 defect in the direction that loses data (#4911).
