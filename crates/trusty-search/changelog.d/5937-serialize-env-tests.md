Fixed
- Tests that write process env knobs (redb cache, idle timers, concurrency limits, `FASTEMBED_CACHE_*`, embedderd restarts, memory-policy limits, `TRUSTY_INDEX_DEVICE`, reaper intervals, `TRUSTY_EMBED_WORKERS`) and the denylist tests that read `HOME` now run in the one unnamed `#[serial]` group, replacing three private mutexes and a named key, so a parallel sibling can no longer change the value mid-test (#5937). Test-only.
- A source-scan ratchet fails the build when a `src/**` test writes the environment outside the unnamed `#[serial]` group (#5937). Test-only.
