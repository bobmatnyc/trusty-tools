Fixed
- Tests that write process env knobs (redb cache, idle timers, concurrency limits, `FASTEMBED_CACHE_*`, embedderd restarts) and the denylist tests that read `HOME` now run under `#[serial]` with the other env writers, so a parallel sibling can no longer change the value mid-test (#5937). Test-only.
