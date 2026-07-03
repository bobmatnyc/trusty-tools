//! Session-launch palace-alias registration to heal claude-mpm memory parity.
//!
//! Why: trusty-mpm pins a managed session to the derived `owner-repo` palace slug
//! (e.g. `bobmatnyc-trusty-tools`, via [`trusty_common::derive_palace_id`]), but
//! the pre-existing claude-mpm-era palace for a repo is the BARE repo name
//! (`trusty-tools`). When the `owner-repo` palace was never created, every memory
//! tool call in the session fails with "palace metadata missing" while the real
//! history lives under the bare name — memory is split-brained (issue #1939). The
//! maintainer's chosen fix is to ALIAS both names, so this module registers a
//! palace-level alias `owner-repo -> bare-repo` at launch when — and only when —
//! that split-brain condition holds.
//!
//! What: [`maybe_register_palace_alias`] resolves the effective git remote,
//! derives the owner-repo and bare names, and (guarded on an absent owner-repo
//! palace plus a present bare palace, and no operator override) persists the
//! alias via [`trusty_common::palace_alias::PalaceAliasStore`]. It is best-effort
//! and side-effect-only: every failure is swallowed with at most a `tracing` line
//! so the launch and palace pinning proceed unchanged.
//!
//! Test: `tests` in this module cover the split-brain registration and the two
//! no-op guards (owner-repo already exists; no bare palace).

use std::path::Path;

use super::settings::git_remote_origin;

/// Register a palace-level alias `owner-repo -> bare-repo` when needed (issue #1939).
///
/// Why: heals the claude-mpm memory split-brain (see module docs) by making the
/// pinned `owner-repo` palace name resolve to the existing bare-repo store,
/// WITHOUT renaming or recreating either palace. Registering only in the true
/// split-brain case keeps `owner-repo` canonical whenever it already exists (or
/// when there is no bare palace to inherit), exactly as before.
/// What: bails when an operator `TRUSTY_MEMORY_PALACE` override is set (the name
/// was chosen deliberately). Otherwise resolves the effective remote (explicit
/// arg, else `git remote get-url origin` under `project_path`), derives the
/// owner-repo ([`trusty_common::owner_repo_from_git_remote`]) and bare
/// ([`trusty_common::repo_slug_from_git_remote`]) names, and — when they differ,
/// the owner-repo palace is missing on disk, and the bare palace exists — calls
/// [`trusty_common::palace_alias::PalaceAliasStore::register_alias`] in the
/// trusty-memory registry dir ([`trusty_common::palace_alias::default_palace_registry_dir`]).
/// Never returns a value or errors; failures are logged and ignored.
/// Test: `creates_alias_for_split_brain`, `noop_when_owner_repo_exists`,
/// `noop_when_no_bare_palace`, `noop_when_override_set`.
pub(super) fn maybe_register_palace_alias(project_path: &Path, git_remote: Option<&str>) {
    // An explicit operator override means the palace name was chosen
    // deliberately — do not second-guess it with an alias.
    if trusty_common::palace_override_from_env().is_some() {
        return;
    }

    let probed = match git_remote {
        Some(_) => None,
        None => git_remote_origin(project_path),
    };
    let Some(remote) = git_remote.map(str::to_string).or(probed) else {
        return;
    };

    let Some(owner_repo) = trusty_common::owner_repo_from_git_remote(&remote) else {
        return;
    };
    let Some(bare) = trusty_common::repo_slug_from_git_remote(&remote) else {
        return;
    };
    // No owner segment (owner_repo == bare) — nothing to alias.
    if owner_repo == bare {
        return;
    }

    let registry_dir = match trusty_common::palace_alias::default_palace_registry_dir() {
        Ok(dir) => dir,
        Err(e) => {
            tracing::debug!(
                error = %e,
                "skipping palace-alias registration: cannot resolve registry dir"
            );
            return;
        }
    };

    let owner_repo_exists = registry_dir.join(&owner_repo).join("palace.json").exists();
    let bare_exists = registry_dir.join(&bare).join("palace.json").exists();
    // Only heal the split-brain: owner-repo missing AND bare present.
    if owner_repo_exists || !bare_exists {
        return;
    }

    match trusty_common::palace_alias::PalaceAliasStore::register_alias(
        &registry_dir,
        &owner_repo,
        &bare,
    ) {
        Ok(()) => tracing::info!(
            alias = %owner_repo,
            target = %bare,
            "registered palace-level alias to heal claude-mpm memory parity (#1939)"
        ),
        Err(e) => tracing::warn!(
            alias = %owner_repo,
            target = %bare,
            error = %e,
            "failed to register palace-level alias (#1939); session will pin owner-repo unaliased"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;
    use trusty_common::palace_alias::PalaceAliasStore;

    /// A GitHub remote whose derived names are `bobmatnyc-trusty-tools` (owner-repo)
    /// and `trusty-tools` (bare) — the exact split-brain pair from issue #1939.
    const REMOTE: &str = "git@github.com:bobmatnyc/trusty-tools.git";
    const OWNER_REPO: &str = "bobmatnyc-trusty-tools";
    const BARE: &str = "trusty-tools";

    /// RAII guard that sets/clears an env var for the duration of a `#[serial]`
    /// test and restores the prior value on drop.
    ///
    /// Why: [`maybe_register_palace_alias`] reads the process-global
    /// `TRUSTY_MEMORY_PALACE` (override) and `TRUSTY_DATA_DIR_OVERRIDE` (registry
    /// dir) env vars. Tests must set them deterministically and restore them so
    /// siblings are unaffected; `#[serial]` serialises the mutation.
    /// What: `set`/`clear` snapshot the prior value; `Drop` restores it.
    /// Test: exercised by every test in this module.
    struct EnvGuard {
        key: &'static str,
        prior: Option<String>,
    }
    impl EnvGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let prior = std::env::var(key).ok();
            // SAFETY: env-mutating tests here are tagged `#[serial]`.
            unsafe { std::env::set_var(key, value) };
            Self { key, prior }
        }
        fn clear(key: &'static str) -> Self {
            let prior = std::env::var(key).ok();
            // SAFETY: serialised by `#[serial]`.
            unsafe { std::env::remove_var(key) };
            Self { key, prior }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: serialised by `#[serial]`.
            match &self.prior {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    /// Create `<registry_dir>/<id>/palace.json` (empty is enough — existence is
    /// all the alias gate checks).
    fn make_palace(registry_dir: &Path, id: &str) {
        let dir = registry_dir.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("palace.json"), b"{}").unwrap();
    }

    /// The registry dir resolves to `<data_override>/trusty-memory` (no `palaces/`
    /// subdir created in these tests).
    fn registry_dir(data_override: &Path) -> PathBuf {
        data_override.join("trusty-memory")
    }

    /// Why: the headline #1939 fix — with the bare palace present and the
    /// owner-repo palace absent, launch must register the alias so the pinned
    /// owner-repo name resolves to the bare store.
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn creates_alias_for_split_brain() {
        let data = tempdir().unwrap();
        let _override = EnvGuard::clear("TRUSTY_MEMORY_PALACE");
        let _data = EnvGuard::set("TRUSTY_DATA_DIR_OVERRIDE", data.path());

        let reg = registry_dir(data.path());
        make_palace(&reg, BARE); // bare exists, owner-repo does not.

        maybe_register_palace_alias(Path::new("/unused"), Some(REMOTE));

        assert_eq!(
            PalaceAliasStore::resolve_alias(&reg, OWNER_REPO).unwrap().as_deref(),
            Some(BARE),
            "split-brain must register {OWNER_REPO} -> {BARE}"
        );
    }

    /// Why: when the owner-repo palace already exists there is no split-brain —
    /// registration must be a no-op so owner-repo stays canonical.
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn noop_when_owner_repo_exists() {
        let data = tempdir().unwrap();
        let _override = EnvGuard::clear("TRUSTY_MEMORY_PALACE");
        let _data = EnvGuard::set("TRUSTY_DATA_DIR_OVERRIDE", data.path());

        let reg = registry_dir(data.path());
        make_palace(&reg, BARE);
        make_palace(&reg, OWNER_REPO); // owner-repo already present.

        maybe_register_palace_alias(Path::new("/unused"), Some(REMOTE));

        assert_eq!(
            PalaceAliasStore::resolve_alias(&reg, OWNER_REPO).unwrap(),
            None,
            "no alias should be registered when the owner-repo palace exists"
        );
    }

    /// Why: with no bare palace to inherit there is nothing to alias to — the
    /// owner-repo palace is created fresh as today, so registration is a no-op.
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn noop_when_no_bare_palace() {
        let data = tempdir().unwrap();
        let _override = EnvGuard::clear("TRUSTY_MEMORY_PALACE");
        let _data = EnvGuard::set("TRUSTY_DATA_DIR_OVERRIDE", data.path());

        let reg = registry_dir(data.path());
        std::fs::create_dir_all(&reg).unwrap(); // registry dir exists, no palaces.

        maybe_register_palace_alias(Path::new("/unused"), Some(REMOTE));

        assert_eq!(
            PalaceAliasStore::resolve_alias(&reg, OWNER_REPO).unwrap(),
            None,
            "no alias should be registered when no bare palace exists"
        );
    }

    /// Why: an operator `TRUSTY_MEMORY_PALACE` override is a deliberate choice;
    /// registration must bail even if a split-brain would otherwise be detected.
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn noop_when_override_set() {
        let data = tempdir().unwrap();
        let _override = EnvGuard::set("TRUSTY_MEMORY_PALACE", Path::new("operator-choice"));
        let _data = EnvGuard::set("TRUSTY_DATA_DIR_OVERRIDE", data.path());

        let reg = registry_dir(data.path());
        make_palace(&reg, BARE); // split-brain present...

        maybe_register_palace_alias(Path::new("/unused"), Some(REMOTE));

        assert_eq!(
            PalaceAliasStore::resolve_alias(&reg, OWNER_REPO).unwrap(),
            None,
            "an operator override must suppress alias registration"
        );
    }
}
