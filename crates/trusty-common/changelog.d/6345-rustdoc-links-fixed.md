Fixed

- Seven broken intra-doc links that failed the pre-publish rustdoc gate
  (`scripts/check_rustdoc_links.sh`) and, because `lib.rs` carries
  `#![deny(rustdoc::broken_intra_doc_links)]`, failed `cargo doc -p
  trusty-common` outright — which also took down the pre-publish contract gate,
  since it reads rustdoc JSON from the same build. `gui_mcp_client` and
  `redb_open` each carry an outer `///` doc on their `pub mod` line in `lib.rs`
  and an inner `//!` block in their own file; rustdoc resolves the whole
  combined doc in the outer attribute's scope, so the inner block's bare
  `[\`running_binary_path\`]`, `[\`build_entry\`]`, `[\`configure\`]`,
  `[\`is_incompatible_format\`]`, `[\`INCOMPATIBLE_SUFFIX\`]`,
  `[\`incompatible_backup_path\`]` and `[\`backup_incompatible_file\`]` were
  looked up at the crate root and found nothing. All seven are now
  `crate::`-qualified, which resolves from either scope.
