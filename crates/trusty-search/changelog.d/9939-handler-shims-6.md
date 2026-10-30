Changed
- The create-index collision and allowlist tests now call `create_index_report`, the core the socket serves, through a `create_answer` test helper instead of the test-only `create_index_handler` shim (#9939, no runtime change).
