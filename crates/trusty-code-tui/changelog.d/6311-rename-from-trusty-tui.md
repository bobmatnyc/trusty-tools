Changed

- The crate is renamed from `trusty-tui` to `trusty-code-tui`, and its
  directory moves from `crates/trusty-tui/` to `crates/trusty-code-tui/`
  (#6311). The Rust path prefix follows: `trusty_tui::TuiEngine` is now
  `trusty_code_tui::TuiEngine`. No item changed name, signature, or behaviour.
- `publish = false` is retained. The crate has never been uploaded to
  crates.io and stays unpublished until it is ready, so the rename claims no
  registry name and reverses nothing.
