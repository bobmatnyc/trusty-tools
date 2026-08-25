Fixed

- **`PruneFilter::Unresolvable`'s doc comment now links to a symbol that actually exists.** The link pointed to `super::record::SessionRecord::workspace_unresolvable`, but `prune_types.rs` loads as the `types` submodule of `prune` (via `#[path = "prune_types.rs"]`), so `super` resolved to `prune`, which has no `record` submodule — `cargo doc`'s broken-intra-doc-link gate failed deterministically. The link now uses the crate-absolute path `crate::session_manager::record::SessionRecord::workspace_unresolvable` ([#6118](https://github.com/bobmatnyc/trusty-tools/issues/6118))
