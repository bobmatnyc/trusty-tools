Removed
- `CodeIndexer::remove_chunk` is removed. It deleted from redb warn-only and rebuilt the whole symbol graph per call, and nothing outside the tests called it; the watcher already removes chunk ids through a fail-closed path (#9212).
