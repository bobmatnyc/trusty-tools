Fixed
- `execute_errors_without_credentials` runs `#[serial]` inside the shared
  credential sandbox instead of setting `HOME` by hand, so it can no longer
  race the granola sandbox test and read the real `~/.gworkspace-mcp`
  token store (#9438).
