//! Shared per-session git-worktree conventions (issue #2032, extended #4270).
//!
//! Why: modules on BOTH sides of the `daemon` feature gate create and destroy
//! per-session worktrees, and every one of them must agree on the same
//! conventions. `daemon::managed_routes::inproject` (feature = `"daemon"`)
//! creates them, `session_manager::decommission` (unconditional) removes them,
//! and since #4270 `provisioner::workspace` (also unconditional) creates the
//! identical topology from the clone-from-URL path. Placing the conventions
//! here, in `core` (always compiled, no feature gate), lets the unconditional
//! modules depend on them WITHOUT pulling in the feature-gated `daemon` module.
//! The reverse dependency direction is what caused the bug this module was
//! created to fix: `decommission.rs` hardcoded `git branch -D <leaf>`, missing
//! the `session/` prefix `create_session_worktree` actually uses, so the
//! branch-delete step targeted a nonexistent ref and silently no-opped — every
//! session's branch leaked forever.
//! What: [`worktree_branch_for`] builds the `session/<name>` branch name for a
//! given worktree leaf/session name; [`ensure_worktrees_gitignored`] keeps the
//! `.worktrees/` directory out of the base clone's `git status` and out of
//! reach of `git clean -ffd`.
//! Test: `worktree_branch_for_adds_session_prefix`,
//! `ensure_worktrees_gitignored_idempotent`,
//! `worktrees_exclude_entry_protects_against_double_force_clean`.

/// Compute the per-session branch name for a worktree leaf/session name.
///
/// Why: single source of truth for the `session/<name>` convention, shared by
/// `daemon::managed_routes::inproject::create_session_worktree` (which
/// creates the branch) and `session_manager::decommission::remove_session_worktree`
/// (which must delete the SAME branch — see the #2032 branch-prefix fix in
/// that module's doc comment).
/// What: `format!("session/{name}")`.
/// Test: `worktree_branch_for_adds_session_prefix`.
pub fn worktree_branch_for(name: &str) -> String {
    format!("session/{name}")
}

/// Add `.worktrees/` to `base_path`'s `.git/info/exclude`, idempotently.
///
/// Why: this is a DATA-LOSS guard, not cosmetics. Without the entry the
/// per-session worktrees are untracked content in the base clone, and
/// `git clean -ffd` there deletes them outright — uncommitted session work
/// included. Single-force `clean` is safe (git refuses to descend into a
/// nested repository, reporting `Skipping repository .worktrees/<id>`), which
/// is why this reads as harmless until someone force-cleans a dirty base. With
/// the entry, `.worktrees/` is ignored and `-ffd` removes nothing. Measured
/// against real git 2.54.0 in a throwaway repo built to the shipping topology:
///
/// | command | without entry | with entry |
/// |---|---|---|
/// | `git clean -fd` / `-xfd` | `Skipping repository` | untouched |
/// | `git clean -ffd` | `Removing .worktrees/` | untouched |
/// | `git clean -xffd` | `Removing .worktrees/` | `Removing .worktrees/` |
///
/// `-xffd` defeats it because `-x` discards ignore rules; nothing short of not
/// running that command protects against it. The secondary benefit is the one
/// this started as: `git status` in the base stays clean.
///
/// Writing to `.git/info/exclude` rather than `.gitignore` keeps the entry
/// local to the clone and never touches a committed file.
/// What: creates `<base>/.git/info/` if absent, then appends the entry only
/// when it is not already present. The read-then-append window means
/// concurrent callers racing at first launch may each observe an absent entry
/// and both append, producing a duplicate line; git tolerates duplicate
/// patterns (the same set is matched), so this is safe, and single-caller
/// invocations are strictly idempotent. Returns `Ok(())` on success or when
/// the entry already exists; propagates I/O errors.
/// Test: `ensure_worktrees_gitignored_idempotent`,
/// `worktrees_exclude_entry_protects_against_double_force_clean`.
pub fn ensure_worktrees_gitignored(base_path: &std::path::Path) -> Result<(), String> {
    // The entry is the shared directory name plus a trailing slash, so the
    // pattern can never drift from the directory the provisioners create.
    // #5204: gitignore the base new worktrees are actually created under.
    let entry_pattern = format!(
        "{}/",
        crate::session_manager::decommission::worktrees_dirname()
    );

    let info_dir = base_path.join(".git").join("info");
    std::fs::create_dir_all(&info_dir)
        .map_err(|e| format!("could not create .git/info/ at {}: {e}", info_dir.display()))?;

    let exclude_path = info_dir.join("exclude");
    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();

    // Idempotent: skip if the entry is already present.
    if existing.lines().any(|line| line.trim() == entry_pattern) {
        return Ok(());
    }

    // Append with a leading newline when the file does not already end with one.
    let entry = if existing.is_empty() || existing.ends_with('\n') {
        format!("{entry_pattern}\n")
    } else {
        format!("\n{entry_pattern}\n")
    };

    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude_path)
        .map_err(|e| format!("could not open .git/info/exclude: {e}"))?;
    file.write_all(entry.as_bytes())
        .map_err(|e| format!("could not write .git/info/exclude: {e}"))?;

    tracing::info!(
        path = %exclude_path.display(),
        "added {entry_pattern} to .git/info/exclude"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The branch convention must always be `session/<name>`, for both
    /// pre-#2032 raw-UUID leaves and the new semantic-tmux-name leaves — the
    /// convention is name-format-agnostic.
    /// Test: this function IS the test.
    #[test]
    fn worktree_branch_for_adds_session_prefix() {
        assert_eq!(worktree_branch_for("tm-foo-01"), "session/tm-foo-01");
        assert_eq!(
            worktree_branch_for("00000000-old-session-uuid"),
            "session/00000000-old-session-uuid"
        );
    }
}
