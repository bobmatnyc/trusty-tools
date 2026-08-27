Changed

- The `trusty-tui` dependency is renamed to `trusty-code-tui` (#6311), and the
  `tcode tui` client's imports move from `trusty_tui::` to
  `trusty_code_tui::`. The dependency still resolves to the same in-workspace
  path crate at the same version, so `tcode tui` behaves exactly as before.
