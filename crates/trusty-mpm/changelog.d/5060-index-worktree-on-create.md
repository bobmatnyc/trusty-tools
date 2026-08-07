Added

- a worktree is now registered with trusty-search at the moment it is created, instead of only when a session launches in it ([#5060](https://github.com/bobmatnyc/trusty-tools/issues/5060))
  - covers both worktree-minting paths: the in-project managed-session path (`daemon::managed_routes::inproject::create_session_worktree`) and the clone-provisioned path (`provisioner::workspace`'s `RealGitBackend::worktree_add`)
  - the index is BM25 + KG only — no embeddings. Exact text and the symbol graph are branch-specific and must be worktree-accurate; conceptual similarity is not, so the expensive lane stays on the base checkout
  - fire-and-forget on a detached thread: worktree creation and session launch never wait on indexing at any repo size. A cold BM25+KG build of this workspace (5,865 files, 75,282 chunks) measured 35.0 s
  - idempotent — a repeat call against an already-indexed worktree is two short HTTP round trips and no walk
  - only a real git worktree is indexed; a plain clone is refused, so worktree creation never makes an operator's opt-in decision for them
  - teardown is unchanged: `session_manager::search_gc` already derives the same index id and deletes it at decommission, with an orphan sweep behind it
