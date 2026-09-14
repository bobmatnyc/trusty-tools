Fixed

- `file_lock`'s module doc links to `DEFAULT_LOCK_TIMEOUT` and `LockTimeout`
  now resolve at rustdoc's default (no-feature) documentation pass. Both
  links previously relied on ordinary intra-doc resolution, which rustdoc
  attributes to the merged doc block at the `pub mod file_lock;` declaration
  site rather than the module's own scope; explicit reference-link
  definitions (matching the existing ones for `with_exclusive_lock` and
  `lock_path`) sidestep that misattribution (#7910).
