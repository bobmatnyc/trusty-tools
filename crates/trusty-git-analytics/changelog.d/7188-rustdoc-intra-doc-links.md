Fixed

- Resolved the three remaining broken rustdoc intra-doc links in `collect::linear::client` and `collect::linear::sync` — the `unchecked_transaction`/`Connection::transaction` mentions are now plain backticks (external crate item), and `build_jql` now carries a link-reference definition to `crate::collect::jira::sync::build_jql` (#7188).
