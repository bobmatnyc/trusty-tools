Documentation
- `resolve_gh_account_env_for_registry` still pointed at
  `find_pinned_gh_account`, which #5864 renamed to `find_pinned_gh_identity`.
  The doc names the current function now (#5973).
- `PickerChoice::LaunchNew` linked `LaunchIsolation::OwnWorktree` by bare name,
  but `session_picker` never imports that type — it re-exports four other items
  from `picker_launch_new` and not this one. The link carries the full path now.
  The failing lib doc had been hiding it: rustdoc never reached the `tm` binary
  while `gh_account.rs` still failed (#5973).
