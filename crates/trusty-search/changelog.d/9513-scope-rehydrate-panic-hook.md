Fixed
- The test-only commit-phase panic hook in the corpus rehydrate is now armed per indexer, so `warm_corpus_rehydrates_an_evicted_index` no longer fails when the rehydrate panic tests run in the same test process (#9513). Release builds are unchanged.
