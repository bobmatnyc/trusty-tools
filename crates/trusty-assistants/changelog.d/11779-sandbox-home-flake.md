Fixed
- `test_process_home_is_a_sandbox` no longer flakes: it holds every lock a `TA_ASSISTANTS_DIR` or other scrubbed-variable mutator holds, and every `TA_ASSISTANTS_DIR` mutator and reader now shares the `#[serial]` key.
