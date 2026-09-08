Fixed

- Resolved the two remaining broken rustdoc intra-doc links in `collectors` — `crate::run::pins` and `crate::chain::audit_with_preflight` are private items, so both mentions are now plain backticks instead of link syntax (#7188).
