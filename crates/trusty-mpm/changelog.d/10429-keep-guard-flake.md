Fixed
- The `keep_guard` build-lease tests no longer fail intermittently, because they now wait a bounded time for the lock instead of racing it (#10429).
