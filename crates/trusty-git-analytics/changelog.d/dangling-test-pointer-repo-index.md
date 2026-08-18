Documentation

- **`probe_args` in `audit/repo_index.rs` now names the tests that actually cover it.** Its `Test:` pointer cited `the_probe_asks_for_the_index_by_id`, a test that was never written, which broke the `Test pointers` CI gate on `main`. It now cites `the_index_invocation_names_the_path_and_the_id`, which asserts the argument vector without spawning, and `an_already_indexed_repository_is_not_reindexed`, which proves the spawned probe is `index-status <id>` and the only invocation for a served repository.
