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
//! Also home to the #3692 auto-suffix-never-reject dedupe surface, for names
//! that are operator-supplied or already resolved rather than derived fresh
//! via `resolve_session_name`'s own per-project serial allocator:
//! [`SessionManager::taken_name_set`] + [`SessionManager::dedupe_name_against`]
//! (the composable pieces `rename`/`adopt_existing` call UNDER their held
//! store write guard) and [`SessionManager::dedupe_session_name`] (the
//! lock-free snapshot wrapper `create_with_resolved_name` uses as the create
//! path's final pre-tmux-create safety net).
//! Test: `naming_tests.rs`.

use std::collections::HashSet;
use std::path::Path;

use super::manager::{ManagedError, SessionManager};
use super::record::ManagedSessionId;

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
    /// so a lost race is an ALIASING risk — two persisted records driving one
    /// physical session — not a crash. `create_with_resolved_name`'s final
    /// pre-create dedupe (#3692) narrows this window to a few instructions
    /// but does NOT close it; the residual is accepted and tracked as issue
    /// #3707 (fixing it properly needs `-A` fail-loud semantics plus a
    /// `TmuxSessionGuard` reaper that can never kill the race winner's
    /// session).
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

    /// Build the daemon-wide "taken" name set: a PRE-FETCHED live tmux
    /// snapshot ∪ every NON-terminal store record's `tmux_name` (issue #3692).
    ///
    /// Why: split from [`Self::dedupe_session_name`] so callers that already
    /// hold the store WRITE guard (`rename`, `adopt_existing` — which must
    /// keep collision-check/dedupe/persist atomic, see the #3692 review's
    /// HIGH-3 finding) can pass the records they read THROUGH that guard,
    /// instead of calling `self.list()` (which takes the same lock →
    /// deadlock) or using a lock-free snapshot (which reintroduces the exact
    /// two-writers-pick-the-same-ordinal race this fixes). `live_tmux_names`
    /// is a parameter — NOT fetched here — because `list_sessions` is a
    /// blocking tmux subprocess and `store.write()` gates every session-
    /// manager mutation daemon-wide (#3698 round-2 HIGH-A): callers must
    /// fetch the snapshot BEFORE taking the guard, never under it.
    /// What: unions `live_tmux_names` with `records`' non-terminal
    /// `tmux_name`s (a `Decommissioned`/`Deleted` tombstone's name is NOT
    /// taken — reusable immediately, unsuffixed), excluding `exclude_id`'s
    /// own current name so a record renaming to a name only it holds is
    /// never suffixed against itself.
    /// Test: exercised via every `dedupe_session_name` /
    /// `rename_suffixes_*` / `manager_adopt_existing_suffixes_*` test.
    pub(crate) fn taken_name_set(
        live_tmux_names: &[String],
        records: &[super::record::SessionRecord],
        exclude_id: Option<&ManagedSessionId>,
    ) -> HashSet<String> {
        let mut taken: HashSet<String> = live_tmux_names.iter().cloned().collect();
        taken.extend(
            records
                .iter()
                .filter(|r| exclude_id != Some(&r.id) && !r.state.is_terminal())
                .map(|r| r.tmux_name.clone()),
        );
        taken
    }

    /// Disambiguate `candidate` against a pre-built taken set — auto-suffix
    /// on collision, NEVER reject (owner decision, issue #3692), respecting
    /// the 64-char session-name cap.
    ///
    /// Why: the pure tail shared by BOTH dedupe surfaces (the lock-free
    /// [`Self::dedupe_session_name`] snapshot and the under-write-guard
    /// `rename`/`adopt_existing` paths). Also owns the length guard the
    /// #3692 review's LOW finding flagged: `validate_session_name` enforces
    /// the 64-char cap on the BARE candidate, so a suffix appended here
    /// could silently push past it.
    /// What: runs [`trusty_common::session_naming::dedupe_by_ordinal`]
    /// (candidate unchanged when free; else smallest free `-N` ordinal). If
    /// the suffixed result would exceed [`MAX_SESSION_NAME_LEN`], truncates
    /// the candidate to reserve [`SUFFIX_HEADROOM`] chars (enough for the
    /// worst-case `-<millis>` fallback suffix) and re-dedupes — the result
    /// is then guaranteed to fit.
    /// Test: `dedupe_name_against_caps_suffixed_length_at_64` in
    /// `super::naming_tests`; every `rename_suffixes_*` test transitively.
    pub(crate) fn dedupe_name_against(candidate: &str, taken: &HashSet<String>) -> String {
        let result =
            trusty_common::session_naming::dedupe_by_ordinal(candidate, |c| taken.contains(c));
        if result.chars().count() <= MAX_SESSION_NAME_LEN {
            return result;
        }
        // The appended ordinal pushed a near-cap candidate over 64 chars —
        // shorten the base to reserve suffix headroom, then re-dedupe.
        let short: String = candidate
            .chars()
            .take(MAX_SESSION_NAME_LEN - SUFFIX_HEADROOM)
            .collect();
        trusty_common::session_naming::dedupe_by_ordinal(&short, |c| taken.contains(c))
    }

    /// Disambiguate `candidate` against every live/tracked name — auto-suffix
    /// on collision, NEVER reject (owner decision, issue #3692: two distinct
    /// managed sessions were found sharing one name, making name-based resume
    /// ambiguous and the picker show duplicate rows).
    ///
    /// Why: [`Self::resolve_session_name`] already guarantees a fresh,
    /// collision-free `tm-<leaf>-NN` name for the derive-from-scratch create
    /// path via its own per-project serial allocator — but its snapshot and
    /// the eventual `tmux new-session` are separated by real work (clone,
    /// worktree provisioning), the documented TOCTOU window. This method is
    /// the create path's FINAL safety net, called by
    /// `create_with_resolved_name` immediately before the tmux create to
    /// re-check-and-suffix the name at the last possible moment, narrowing
    /// (not fully closing — that would need a lock spanning the external
    /// tmux/store processes) that window.
    /// NOTE for other callers: `rename` and `adopt_existing` must NOT use
    /// this snapshot wrapper — they serialize their check/dedupe/persist
    /// against the store write guard (tmux calls kept OUTSIDE it, with a
    /// re-verify before persisting) and go through [`Self::taken_name_set`]
    /// plus [`Self::dedupe_name_against`] directly (calling this here would
    /// deadlock on the store lock, and a snapshot would reintroduce the
    /// #3692 race for two concurrent stopped-session renames).
    /// Residual (ACCEPTED, tracked as issue #3707): because this is a
    /// lock-free snapshot and `create_session` shells out to
    /// `tmux new-session -A -d`, two truly concurrent creators that compute
    /// the same deduped name both "succeed" — `-A` silently ATTACHES the
    /// loser to the winner's session, persisting two records that alias one
    /// physical pane. Closing that needs `-A` semantics + `TmuxSessionGuard`
    /// reaper changes (the loser must never kill the winner's session) — see
    /// #3707 for the plan; the docs here exist so the acceptance is loud,
    /// not silent.
    /// What: [`Self::taken_name_set`] over a fresh tmux `list_sessions`
    /// snapshot (fetched BEFORE `self.list()` takes the store lock — never
    /// under it, #3698 round-2 HIGH-A) + a fresh `self.list()` snapshot,
    /// then [`Self::dedupe_name_against`].
    /// Test: `create_with_reserved_name_suffixes_stale_reservation` in
    /// `super::naming_tests`.
    pub(crate) async fn dedupe_session_name(
        &self,
        candidate: &str,
        exclude_id: Option<&ManagedSessionId>,
    ) -> Result<String, ManagedError> {
        let live_names = self
            .tmux
            .list_sessions()
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))?;
        let records = self.list().await;
        let taken = Self::taken_name_set(&live_names, &records, exclude_id);
        Ok(Self::dedupe_name_against(candidate, &taken))
    }
}

/// Cap on a managed session name's length — must match the 64-char rule
/// [`super::rename::validate_session_name`] enforces on operator input, so an
/// auto-appended collision suffix (#3692) can never push a validated name
/// back over the limit tmux/we accept.
pub(crate) const MAX_SESSION_NAME_LEN: usize = 64;

/// Headroom [`SessionManager::dedupe_name_against`] reserves when truncating
/// a near-cap candidate: covers the worst-case `-<millis>` timestamp fallback
/// suffix (1 + 13 digits) with margin.
const SUFFIX_HEADROOM: usize = 16;
