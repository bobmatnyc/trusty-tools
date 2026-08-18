Documentation
- `stable_set.rs` referred to the report module as `super::install_report`. The
  module is declared inside `install.rs`, so its real path is
  `commands::install::install_report` and the link resolved to nothing. Both
  references are plain code text now (#5973).
