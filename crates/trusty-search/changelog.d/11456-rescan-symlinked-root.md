Fixed
- A watcher rescan reconcile given its root through a symlink alias (macOS `/var` -> `/private/var`) now indexes the changed files; it returned Ok with zero stats and indexed nothing (#11456).
