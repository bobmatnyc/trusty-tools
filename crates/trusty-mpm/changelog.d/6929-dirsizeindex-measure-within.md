Added

- `DirSizeIndex::measure_within(root, budget)` measures a directory under a caller-supplied wall-clock budget (#6929). `IndexPolicy::walk_budget` is a fixed ceiling, so a caller working to a deadline of its own had no way to bound one walk. The per-call budget can only tighten that bound — the walk runs for the shorter of the two — and `measure` is now `measure_within(root, None)`, unchanged for every existing caller.
