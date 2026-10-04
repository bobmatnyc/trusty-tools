Fixed

- `execute_errors_when_api_key_missing` runs `#[serial]` inside the shared
  credential sandbox, which restores `GRANOLA_API_KEY` even when an assertion
  fails (#9123).
