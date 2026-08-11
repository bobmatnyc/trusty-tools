Fixed
- Test isolation: the two tests that mutate `TRUSTY_HNSW_REVIEW_IDLE` moved out of the lib test binary into their own integration binary, so they can no longer make `hnsw_idle_demotion_reviews_clean_promoted_store` fail by holding the process-global gate at `0` while it runs (#3769).
