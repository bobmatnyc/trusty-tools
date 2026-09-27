Fixed
- Tests that write process env (bm25 knobs, `FASTEMBED_CACHE_*`, `TRUSTY_DATA_DIR_OVERRIDE`, `TRUSTY_MEMORY_PALACE`, `TRUSTY_DREAM_DISABLED`, the idle-evict and bm25-lane knobs) and the tests that read those paths now share one lock, so a parallel sibling can no longer point a test at the wrong data dir or palace (#5937). Test-only.
- A source-scan ratchet fails the build when a lib test writes the environment without `commands::env_test_lock()`; known exceptions are listed with a reason (#5937). Test-only.
