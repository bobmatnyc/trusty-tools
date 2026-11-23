Fixed
- The `token` watcher now holds an exclusive `flock` on `token-watch-state.json.lock` from load to save, and saves through a per-process temp file, so a concurrent Python `token_watch.py` writer no longer loses its offsets or fired keys. A lock it cannot take within 30 s fails the scan instead of writing unlocked (#11629).
