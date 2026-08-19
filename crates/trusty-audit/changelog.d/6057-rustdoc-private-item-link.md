Documentation

- `run::github_issues`' module docs no longer fail `cargo doc`. The header linked
  `crate::clone::record_selection`, which is a private `fn`, and rustdoc cannot
  resolve a link to a private item from a public doc — under
  `-D rustdoc::broken_intra_doc_links` that is an error, not a warning. It is now
  plain backticks, which says the same thing and resolves everywhere.
