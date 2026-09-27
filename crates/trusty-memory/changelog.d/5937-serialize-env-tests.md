Fixed
- Tests that write process env (bm25 knobs, `FASTEMBED_CACHE_*`, `TRUSTY_DATA_DIR_OVERRIDE`, `TRUSTY_MEMORY_PALACE`, `TRUSTY_DREAM_DISABLED`) and the tests that read those paths now share one lock, so a parallel sibling can no longer point a test at the wrong data dir or palace (#5937). Test-only.
