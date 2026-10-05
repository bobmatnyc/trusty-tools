Fixed
- `PalaceStore::load_palace` uses the directory it read `palace.json` from as the palace's `data_dir`, not the absolute path recorded at creation. A palace root copied elsewhere no longer opens the original palace's files.
- A second in-process open of a live palace shares the first handle's recall log (`RecallLog::open_shared`) instead of failing with "Database already open" and running with recall analytics disabled.
- A cold palace open replays its stored vectors into the HNSW graph in parallel, which was the largest cost of a cold open (#9141).
