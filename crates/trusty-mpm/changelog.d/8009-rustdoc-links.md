Documentation

- Three rustdoc intra-doc links in `savings_instructions.rs`,
  `savings_sidecar.rs`, and `catchup_superseded.rs` pointed at `pub(crate)`
  items invisible to a public `cargo doc` build; switched to plain backticks
  so the rustdoc gate resolves them
  (refs [#8009](https://github.com/bobmatnyc/trusty-tools/issues/8009))
