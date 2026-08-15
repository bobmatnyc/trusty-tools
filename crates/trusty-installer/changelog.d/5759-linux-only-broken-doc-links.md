Fixed
- Stopped 14 doc comments in non-gated code from linking to macOS-gated items
  (`has_developer_id_cert`, `sign_binary`, `verify_signature`, `sign_set_strict`,
  `current_identifier`, `post_install_signed_set`, `apply_not_loaded_fallback`,
  `attempt_verify_fallback`), which rustdoc cannot resolve on Linux. docs.rs
  builds on Linux once per release and never rebuilds, so these would have been
  baked into the published documentation permanently.
