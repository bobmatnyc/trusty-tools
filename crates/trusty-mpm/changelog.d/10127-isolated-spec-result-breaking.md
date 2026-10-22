Breaking
- `runtime::cli_launch::isolated_spec` now returns `Result`, so a failed Architect lockdown build stops the launch instead of skipping the lockdown; callers propagate the error with `?` (#10127).
