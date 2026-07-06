//! Tmux session-name resolution, extracted from `manager.rs` (issue #2032).
//!
//! Why: `manager.rs` was at the 500-SLOC production cap (same constraint
//! documented in `reactivate.rs`/`hook_sync.rs`), and issue #2032 needs the
//! semantic tmux name resolved BEFORE the in-project per-session git worktree
//! is created — the worktree directory and branch are now named after the
//! tmux name instead of the raw session UUID, so the name has to exist first.
//! Splitting the derivation/serial-allocation/collision-retry loop out of
//! `create_with_id` (moved here verbatim, then generalised) lets BOTH
//! `create_with_id` (post-hoc derivation, for the cloned/local spawn paths)
//! and the in-project spawn routing layer (pre-hoc reservation, via
//! `SessionManager::create_with_reserved_name`) share the exact same logic —
//! a session's tmux name is derived in exactly ONE place, never twice.
//! What: `SessionManager::resolve_session_name` runs the existing
//! priority-ordered derivation (`name_hint` > GitHub-parseable `repo_url`-
//! derived project > `cwd` basename), the per-project serial allocation
//! (issue #1955), and the bounded collision-retry loop (#1966 review
//! follow-up). The `extra_collision` predicate lets a caller reject a
//! candidate for reasons `resolve_session_name` cannot see itself — the
//! in-project spawn path uses it to also refuse names whose
//! `.worktrees/<name>` directory or `session/<name>` branch already exists on
//! disk (a collision surface the plain tmux-name check knows nothing about;
//! see `daemon::managed_routes::inproject::create_session_worktree`).
//! Test: `naming_tests.rs`.

use std::path::Path;

use super::manager::{ManagedError, SessionManager};

impl SessionManager {
    /// Resolve a fresh, collision-free tmux session name (issues #1955, #2032).
    ///
    /// Why: both [`Self::create_with_id`] (deriving the name itself) and the
    /// in-project spawn path (reserving the name BEFORE the worktree that will
    /// be named after it) need identical priority/serial/collision-retry
    /// semantics. Centralising it here means neither caller can silently drift
    /// from the other's naming behaviour.
    /// What: name selection priority —
    ///   1. Explicit `name_hint` → leaf = sanitized hint (treated as a
    ///      directory-like basename) → `tm-<hint-slug>-NN`.
    ///   2. GitHub-parseable `github_project` (a bare repo name, already
    ///      extracted by the caller from `repo_url`) → `tm-<repo>-NN`.
    ///   3. Otherwise → leaf = basename(`cwd`), or `"local"` if that sanitizes
    ///      to empty.
    ///
    /// Every branch allocates a per-project serial so concurrent sessions for
    /// the same leaf get distinguishable `-01`, `-02`, `-03` names. A candidate
    /// is rejected (and retried with the next free serial) when it collides
    /// with a live tmux session OR when `extra_collision(candidate)` returns
    /// `true` — the latter lets the in-project path fold in its
    /// worktree-dir/branch existence check without this function needing to
    /// know anything about git worktrees. Returns [`ManagedError::NameCollision`]
    /// if every attempt in the bounded window (`MAX_NAME_ALLOCATION_ATTEMPTS`)
    /// collides.
    ///
    /// Residual race window (KNOWN, not closed by this loop, unchanged from the
    /// pre-#2032 behaviour): there is still a gap between THIS check and the
    /// caller's actual `create_session`/`git worktree add` a few lines later.
    /// `create_session` shells out to `tmux new-session -A -d`, and `-A` makes
    /// tmux ATTACH to an existing session of the same name instead of failing,
    /// so a lost race is an aliasing risk, not a crash. Closing this fully
    /// would require a distributed lock across the external `tmux`/`git`
    /// processes — out of scope; tracked as a follow-up if it proves to matter.
    /// Test: `manager_serial_reuses_decommissioned_gap`,
    /// `resolve_session_name_rejects_extra_collision` in `naming_tests.rs`.
    pub(crate) async fn resolve_session_name(
        &self,
        name_hint: Option<&str>,
        github_project: Option<&str>,
        cwd: &Path,
        extra_collision: impl Fn(&str) -> bool,
    ) -> Result<String, ManagedError> {
        let mut existing_names = self.names_for_serial_allocation().await?;
        let leaf_hint = name_hint.map(|h| crate::core::names::leaf_slug_from_dir(Path::new(h)));

        const MAX_NAME_ALLOCATION_ATTEMPTS: u8 = 5;
        for _ in 0..MAX_NAME_ALLOCATION_ATTEMPTS {
            let candidate = if let Some(ref leaf) = leaf_hint {
                crate::core::names::build_session_name(leaf, &existing_names)?
            } else {
                crate::core::names::build_managed_session_name(
                    github_project,
                    cwd,
                    &existing_names,
                )?
            };
            if self.tmux.session_exists(&candidate) || extra_collision(&candidate) {
                existing_names.push(candidate);
                continue;
            }
            return Ok(candidate);
        }
        Err(ManagedError::NameCollision(format!(
            "no free `tm-<leaf>-NN` name after {MAX_NAME_ALLOCATION_ATTEMPTS} attempts \
             (sustained concurrent creates for the same project)"
        )))
    }
}
