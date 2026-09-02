Fixed

- Index-id derivation no longer falls back to the home directory's basename
  (#6550). `resolve_project_root` returns the start path when nothing above it
  is a git repository, so a registration handed `$HOME` created an index named
  after the operator — the live daemon held index `masa` for an unrelated
  repository. `ensure_project_indexed_reporting` and the incremental
  `index_files_best_effort` path now refuse a root that is the home directory
  or the filesystem root, log at error, and return no id, so no caller can pin
  one. New shared predicate `trusty_common::refuse_unindexable_root` (and its
  pure `*_against` form) is the one implementation trusty-search's
  `detect_project` routes through too.
  - `IndexRegistration` gains a `RefusedUnindexableRoot(IndexRootRefusal)`
    variant reporting that outcome.
