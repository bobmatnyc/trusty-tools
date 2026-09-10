Fixed
- The `human_bytes` doc comment in the disk module now links `crate::daemon::doctor` and names `doctor_worktree_disk` as plain text, so rustdoc resolves the reference instead of rendering a dead link into published documentation, and the generated `tm-capabilities` doctor reference carries the current `hooks_build_tree_binary` description.
