//! trusty-search project-index registration/reindex helper.
//!
//! Why: split out of `settings.rs` (issue #610 SLOC cap) — the trusty-search
//! index lifecycle (find-or-create the index, then best-effort trigger a
//! reindex so it is actually populated) is a cohesive concern distinct from the
//! output-style/hook/trust-preseed helpers that remain in `settings.rs`.
//! What: [`register_project_index`] is a thin wrapper over the shared
//! [`trusty_common::search_index::ensure_project_indexed`] entry point, which
//! find-or-creates the daemon-side index and ensures it is populated (issue
//! #1908). That register+reindex logic was PROMOTED into trusty-common so
//! trusty-code can reuse the ONE implementation (common entry-point rule) —
//! this module keeps only the wrapper.
//!
//! // #4181: this module also held `trusty_search_mcp_value` /
//! `inject_trusty_search_mcp`, which wrote a `serve --index <id>` stub into the
//! workspace `.mcp.json`. ADR-0042 deleted both — the id this module returns is
//! now exported as `TRUSTY_INDEX` by the spawn instead
//! ([`crate::core::mcp_session_env`]), which `trusty-search serve` honours since
//! #5394.
//! Test: `register_project_index` is covered in `tests_search_index.rs`.

use std::path::Path;

/// Find-or-create the trusty-search index for `project_root`, best-effort
/// trigger a reindex so it is actually populated, and return its id (issues
/// #1373, #1908).
///
/// Why: pinning the serve stub to an index id is only useful if that index
/// actually exists in the daemon — otherwise a query against it returns nothing
/// and the LLM falls back to guessing (the very bug #1373 fixes). The
/// register-and-populate logic was PROMOTED into
/// [`trusty_common::search_index::ensure_project_indexed`] so trusty-code can
/// reuse the ONE implementation (common entry-point rule); this wrapper keeps
/// the session-launch call site and name stable. Derive the canonical index id,
/// best-effort `POST /indexes` + freshness-gated reindex when the daemon is
/// reachable, and never propagate an error (the session must still launch).
///
/// `None` on an unconfirmed registration (#5091): the shared helper used to hand
/// the derived id back even when the create failed or never happened, so this
/// wrapper pinned `.mcp.json` to an index the daemon had never heard of and
/// every `search` in that session answered `404 unknown index` — permanently,
/// invisibly, with the `search` health check still green. It now returns the id
/// only when the daemon confirmed it; otherwise the caller writes the unpinned
/// stub, which `tm doctor`'s `search_index_pin` check reports as a warning
/// instead of a silent dead end.
/// What: delegates to [`trusty_common::search_index::ensure_project_indexed`]
/// with `allow_sensitive_path: false` (issue #2914). A trusty-mpm session
/// workspace is always either the user's checked-out repository or a
/// `.worktrees/<uuid>` leaf inside it — never a legitimate OS-temp path — so
/// this caller, unlike trusty-code's task-start caller, has no reason to
/// bypass the daemon's `SENSITIVE_PATH_PREFIXES` denylist. Before this fix the
/// bypass was unconditional, so a session-launch test standing in a `tempfile`
/// fixture for `project_root` (e.g. a workspace under `/var/folders/…`)
/// silently registered that throwaway directory against whatever trusty-search
/// daemon happened to be running on the developer/CI machine — the root cause
/// of the ephemeral-index leak this issue reports.
///
/// `skip_vector` (#5065 review): forwarded from [`worktree_skip_vector`] rather
/// than left at its `false` default, so launching a session in a worktree
/// cannot undo the BM25+KG-only decision worktree creation made. See that
/// function for why the ordering made this a real drift and not a theoretical
/// one.
/// Test: `register_project_index_withholds_id_when_registration_is_unconfirmed`
/// (#5091) and `register_project_index_never_bypasses_sensitive_path_denylist`
/// (issue #2914 regression) in `tests.rs`; the promoted logic is unit-tested in
/// `trusty_common::search_index::tests`.
pub(crate) fn register_project_index(project_root: &Path) -> Option<String> {
    let skip_vector = worktree_skip_vector(project_root);
    if skip_vector {
        // #5069: a worktree's index carries no vectors, so launching a session
        // in one must also ensure the base checkout — the facet that owns the
        // embedding lane — actually exists. Worktrees created before #5069 never
        // run the creation-time path again, so launch is the only hook they hit.
        crate::core::base_facet_index::ensure_base_facet_indexed_in_background(
            trusty_common::resolve_project_root(project_root),
        );
    }
    trusty_common::search_index::ensure_project_indexed_with(
        project_root,
        trusty_common::search_index::IndexOptions::default().with_skip_vector(skip_vector),
    )
}

/// Should the index for `project_root` be registered BM25+KG-only (#5065 review)?
///
/// Why: BM25+KG-only was not an invariant, only the behaviour of whichever call
/// happened to land first. #5060 registers a worktree's index with
/// `skip_vector: true` at CREATION, but session launch reached the same
/// worktree path through [`register_project_index`] with `skip_vector` at its
/// `false` default. `POST /indexes` is find-or-create and short-circuits on an
/// existing id, so ordering decided the outcome: whenever creation-time
/// indexing failed, was skipped, or simply lost the race, launch minted a
/// vector-bearing index for the worktree and the daemon persisted that choice.
/// Deciding it from the path — the one thing both call sites agree on — removes
/// the ordering dependence entirely.
/// What: resolves the git-root the index will actually be keyed to (the same
/// `resolve_project_root` hop `ensure_project_indexed_with` performs
/// internally) and asks whether THAT directory is a git worktree. Testing
/// `project_root` itself would answer `false` for a session launched from a
/// SUBDIRECTORY of a worktree, whose index is still keyed to the worktree root
/// — reintroducing the same drift through a narrower door.
/// Test: `worktree_skip_vector_true_for_worktree_root`,
/// `worktree_skip_vector_true_from_worktree_subdirectory`,
/// `worktree_skip_vector_false_for_plain_clone`.
pub(super) fn worktree_skip_vector(project_root: &Path) -> bool {
    crate::core::worktree_index::is_git_worktree(&trusty_common::resolve_project_root(project_root))
}
