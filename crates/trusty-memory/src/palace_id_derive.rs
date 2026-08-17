//! Default palace-ID derivation from project identity (issue #1217).
//!
//! Why: the pure derivation core moved into `trusty-common` (issue #1605) so
//! trusty-mpm's managed-session MCP injection can derive the *identical* palace
//! slug it pins via `TRUSTY_MEMORY_PALACE` without taking a trusty-mpm →
//! trusty-memory dependency edge. This module is now a thin re-export shim:
//! every trusty-memory call site (`messaging::operations`, `messaging::mod`)
//! keeps working unchanged via `crate::palace_id_derive::*`, while the single
//! source of truth — and the unit tests for every precedence level / git-URL
//! variant — lives in [`trusty_common::palace_id`].
//!
//! What: re-exports [`derive_palace_id`], [`owner_repo_from_git_remote`],
//! [`parent_dir_slug`], [`PALACE_OVERRIDE_ENV`], and [`palace_override_from_env`],
//! and adds [`follow_palace_alias`] (#5810) — the step after derivation that turns
//! a derived slug into a name the memory tools accept as input.
//!
//! Test: derivation is pinned by `cargo test -p trusty-common -- palace_id::tests`;
//! trusty-memory inherits it transitively (its `messaging` tests exercise the
//! env-override read path that wraps these shims). The alias step has its own
//! tests in this module.
//!
//! [`derive_palace_id`]: trusty_common::palace_id::derive_palace_id
//! [`owner_repo_from_git_remote`]: trusty_common::palace_id::owner_repo_from_git_remote
//! [`parent_dir_slug`]: trusty_common::palace_id::parent_dir_slug
//! [`PALACE_OVERRIDE_ENV`]: trusty_common::palace_id::PALACE_OVERRIDE_ENV
//! [`palace_override_from_env`]: trusty_common::palace_id::palace_override_from_env

pub use trusty_common::palace_id::{
    derive_palace_id, owner_repo_from_git_remote, palace_override_from_env, parent_dir_slug,
    PALACE_OVERRIDE_ENV,
};

/// Follow a palace-level alias so a derived slug becomes a name that resolves.
///
/// Why (#5810): derivation stops at the `owner-repo` slug, and the daemon
/// redirects that to the aliased palace only once a request arrives. Anything
/// that printed the derived name — the `UserPromptSubmit` injection header, the
/// `SessionStart` inbox header — therefore named a palace `palace_list` does not
/// return and `memory_recall` callers cannot copy. Applying the redirect here,
/// once, gives the hooks a name the memory tools accept as input, and makes the
/// name they print the same palace they queried.
/// What: resolves the daemon's registry directory via
/// [`trusty_common::palace_alias::default_palace_registry_dir`] — the same
/// derivation `main.rs` uses for `AppState::data_root`, so the two cannot point
/// at different files — and returns
/// [`trusty_common::palace_alias::alias_target_if_absent`]'s target when a
/// redirect fires. Returns `slug` untouched on every other path, including an
/// unreadable data dir, so a degraded filesystem keeps today's behaviour instead
/// of losing the name entirely. The redirect is a no-op unless the derived
/// palace is absent AND its alias target exists, so a real palace is never
/// renamed.
/// Test: `follow_palace_alias_names_the_target`,
/// `follow_palace_alias_leaves_a_real_palace_alone`,
/// `prompt_context_header_names_the_alias_target`.
pub fn follow_palace_alias(slug: String) -> String {
    let Ok(registry_dir) = trusty_common::palace_alias::default_palace_registry_dir() else {
        return slug;
    };
    trusty_common::palace_alias::alias_target_if_absent(&registry_dir, &slug).unwrap_or(slug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::palace_alias::PalaceAliasStore;

    /// Point the data-dir override at `dir` for the duration of the closure.
    ///
    /// SAFETY: callers hold `crate::commands::env_test_lock`, which serialises
    /// every `TRUSTY_DATA_DIR_OVERRIDE` mutation in this crate's test binary.
    fn with_data_dir<T>(dir: &std::path::Path, f: impl FnOnce() -> T) -> T {
        unsafe { std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, dir) };
        let out = f();
        unsafe { std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV) };
        out
    }

    fn seed_palace(registry_dir: &std::path::Path, id: &str) {
        let dir = registry_dir.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("palace.json"), b"{}").unwrap();
    }

    /// Why (#5810): the whole point — a derived slug with no palace of its own
    /// must come back as the palace the memory tools actually accept.
    /// Test: itself.
    #[tokio::test]
    async fn follow_palace_alias_names_the_target() {
        let _guard = crate::commands::env_test_lock().lock().await;
        let tmp = tempfile::tempdir().unwrap();
        with_data_dir(tmp.path(), || {
            let registry = trusty_common::palace_alias::default_palace_registry_dir().unwrap();
            seed_palace(&registry, "trusty-tools");
            PalaceAliasStore::register_alias(&registry, "bobmatnyc-trusty-tools", "trusty-tools")
                .unwrap();
            assert_eq!(
                follow_palace_alias("bobmatnyc-trusty-tools".to_string()),
                "trusty-tools"
            );
        });
    }

    /// Why: an alias must never rename a palace that exists, and a slug with no
    /// alias at all must survive the round trip unchanged.
    /// Test: itself.
    #[tokio::test]
    async fn follow_palace_alias_leaves_a_real_palace_alone() {
        let _guard = crate::commands::env_test_lock().lock().await;
        let tmp = tempfile::tempdir().unwrap();
        with_data_dir(tmp.path(), || {
            let registry = trusty_common::palace_alias::default_palace_registry_dir().unwrap();
            seed_palace(&registry, "real-palace");
            seed_palace(&registry, "other-palace");
            PalaceAliasStore::register_alias(&registry, "real-palace", "other-palace").unwrap();
            assert_eq!(
                follow_palace_alias("real-palace".to_string()),
                "real-palace"
            );
            assert_eq!(
                follow_palace_alias("never-aliased".to_string()),
                "never-aliased"
            );
        });
    }
}
