//! Which `CARGO_TARGET_DIR` a leased build gets (#8261).
//!
//! Why: the slot index is only half of a slot; the other half is a target
//! directory no concurrent build shares, so cargo's own build-directory lock
//! never serialises two leased builds and one build's artifacts never replace
//! another's. Holding flock `K` for the build's whole life is what makes pool
//! directory `slot-K` exclusive.
//!
//! What: [`SharedDirs`] names every directory concurrent worktrees share: the
//! configured `build.cargo_target_dir`, anything under the default shared root
//! `~/.trusty-tools/cargo-target/`, and anything under `builders.slot_pool_root`
//! (a slot directory some other lease may hold). None of it needs a repo
//! identity (#8261 round 3). [`plan`] is the pure rule: an unset
//! `CARGO_TARGET_DIR` (cargo's `./target`, a repo's `build.target-dir`) and any
//! other pinned directory are left alone; a shared one is replaced by the held
//! slot's directory, and when no pool is available the build is refused.
//! Test: the `#[cfg(test)]` suite below.

use std::path::{Path, PathBuf};

use crate::core::builder_slot_pool::SlotPool;
use crate::core::builders::BuildersConfig;

/// The directories a build must not run in unleased-by-slot.
///
/// Test: `shared_dirs_need_no_repo_identity`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct SharedDirs {
    /// Any path at or under one of these is shared.
    pub roots: Vec<PathBuf>,
}

impl SharedDirs {
    /// The shared directories for this machine's config.
    ///
    /// What: `builders.slot_pool_root`, `~/.trusty-tools/cargo-target`, and the
    /// configured `build.cargo_target_dir` when one is set.
    #[must_use]
    pub fn resolve(config: &BuildersConfig, home: &Path) -> Self {
        let mut roots = vec![
            config.effective_slot_pool_root(home),
            home.join(trusty_common::crate_config::TRUSTY_TOOLS_DIR)
                .join(crate::core::build_env::CARGO_TARGET_SUBDIR),
        ];
        let configured = load_build_config(home)
            .and_then(|b| b.cargo_target_dir)
            .map(|t| trusty_common::workspace_layout::expand_tilde(&t, home));
        roots.extend(configured);
        Self { roots }
    }

    /// Whether `dir` is, or lies under, a shared directory.
    ///
    /// What: compared component-wise, and again after canonicalizing both
    /// sides when both exist (`/tmp` vs `/private/tmp`).
    #[must_use]
    pub fn contains(&self, dir: &Path) -> bool {
        let canon = |p: &Path| std::fs::canonicalize(p).ok();
        self.roots.iter().any(|root| {
            dir.starts_with(root)
                || matches!((canon(dir), canon(root)), (Some(d), Some(r)) if d.starts_with(&r))
        })
    }
}

/// What a leased build does with `CARGO_TARGET_DIR`.
///
/// Test: every test below.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TargetPlan {
    /// The caller pinned this private directory; leave it.
    Keep(String),
    /// Unset: cargo's own target directory stands.
    Unset,
    /// Replace the shared directory with the held slot's, from this pool,
    /// seeding a new slot from `clone_from`.
    Slot {
        /// The repo's slot pool.
        pool: SlotPool,
        /// The repo's warm shared directory, if any.
        clone_from: Option<PathBuf>,
    },
    /// The build would run in a shared directory and no slot can replace it.
    Refuse(String),
}

/// Decide the target directory before a slot is taken.
///
/// What: `ambient` is `CARGO_TARGET_DIR` as the build would inherit it;
/// `pool` is the repo's slot pool and warm directory, or why there is none.
/// Test: `an_explicit_target_dir_is_kept`, `the_shared_dir_is_replaced_by_the_slot`,
/// `an_unset_target_dir_is_left_to_cargo`, `a_shared_dir_without_a_pool_is_refused`,
/// `an_ambient_pool_slot_counts_as_shared`.
#[must_use]
pub fn plan(
    ambient: Option<&str>,
    shared: &SharedDirs,
    pool: Result<(SlotPool, Option<PathBuf>), String>,
) -> TargetPlan {
    let Some(dir) = ambient.filter(|d| !d.trim().is_empty()) else {
        return TargetPlan::Unset;
    };
    if !shared.contains(Path::new(dir)) {
        return TargetPlan::Keep(dir.to_string());
    }
    match pool {
        Ok((pool, clone_from)) => TargetPlan::Slot { pool, clone_from },
        // #8261 round 3: a shared directory with no pool never builds there.
        Err(why) => TargetPlan::Refuse(format!(
            "CARGO_TARGET_DIR={dir} is a shared target directory that concurrent builds \
             overwrite, and no private slot directory can replace it ({why}). Set a private \
             CARGO_TARGET_DIR for this build, or add a git `origin` to the checkout"
        )),
    }
}

/// The `build:` section of `~/.trusty-tools/trusty-mpm` config, if readable.
fn load_build_config(home: &Path) -> Option<crate::core::build_env::BuildConfig> {
    trusty_common::crate_config::load_at::<crate::core::trusty_tools_config::TrustyToolsConfig>(
        &trusty_common::crate_config::crate_config_path_at(
            home,
            crate::core::trusty_tools_config::CRATE_NAME,
        ),
    )
    .ok()
    .flatten()
    .and_then(|c| c.build)
}

/// The slot pool for `checkout`, and the shared directory to seed it from.
///
/// # Errors
///
/// Why no pool applies: no git origin identity for the checkout.
///
/// Test: `a_checkout_without_an_origin_has_no_pool`.
pub fn resolve_pool(
    config: &BuildersConfig,
    home: &Path,
    checkout: &Path,
) -> Result<(SlotPool, Option<PathBuf>), String> {
    let identity = trusty_common::github_path::derive_github_path(checkout).ok_or_else(|| {
        format!(
            "{} has no git origin identity to key a slot directory by",
            checkout.display()
        )
    })?;
    let shared = crate::core::build_env::resolve_build_env(
        load_build_config(home).as_ref(),
        home,
        Some(&identity),
        crate::core::build_env::host_cores(),
    )
    .ok()
    .map(|env| env.cargo_target_dir);
    Ok((
        SlotPool::new(config.effective_slot_pool_root(home), identity),
        shared,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> SharedDirs {
        SharedDirs {
            roots: vec![PathBuf::from("/shared/target"), PathBuf::from("/pool")],
        }
    }

    fn pool() -> Result<(SlotPool, Option<PathBuf>), String> {
        let id = trusty_common::github_path::GithubPath {
            owner: "o".into(),
            repo: "r".into(),
        };
        Ok((SlotPool::new(PathBuf::from("/pool"), id), None))
    }

    #[test]
    fn an_explicit_target_dir_is_kept() {
        let got = plan(Some("/work/target-mine"), &shared(), pool());
        assert!(matches!(got, TargetPlan::Keep(d) if d == "/work/target-mine"));
    }

    #[test]
    fn the_shared_dir_is_replaced_by_the_slot() {
        for ambient in ["/shared/target", "/shared/target/", "/shared/target/o/r"] {
            let got = plan(Some(ambient), &shared(), pool());
            assert!(
                matches!(got, TargetPlan::Slot { .. }),
                "{ambient:?}: {got:?}"
            );
        }
    }

    #[test]
    fn an_unset_target_dir_is_left_to_cargo() {
        assert!(matches!(plan(None, &shared(), pool()), TargetPlan::Unset));
        assert!(matches!(
            plan(Some(" "), &shared(), pool()),
            TargetPlan::Unset
        ));
    }

    /// #8261 round 3: no repo identity, shared directory ambient — refused.
    #[test]
    fn a_shared_dir_without_a_pool_is_refused() {
        let got = plan(Some("/shared/target"), &shared(), Err("no origin".into()));
        assert!(
            matches!(&got, TargetPlan::Refuse(why) if why.contains("no origin")),
            "{got:?}"
        );
    }

    /// #8261 round 3: another lease's slot directory is shared, not pinned.
    #[test]
    fn an_ambient_pool_slot_counts_as_shared() {
        let got = plan(Some("/pool/o/r/slot-0"), &shared(), pool());
        assert!(matches!(got, TargetPlan::Slot { .. }), "{got:?}");
    }

    #[test]
    fn shared_dirs_need_no_repo_identity() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dirs = SharedDirs::resolve(&BuildersConfig::default(), tmp.path());
        assert!(dirs.contains(&tmp.path().join(".trusty-tools/cargo-target/any/repo")));
        assert!(
            dirs.contains(
                &tmp.path()
                    .join(".trusty-tools/cargo-target-pool/o/r/slot-3")
            )
        );
        assert!(!dirs.contains(&tmp.path().join("private-target")));
    }

    #[test]
    fn a_checkout_without_an_origin_has_no_pool() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = resolve_pool(&BuildersConfig::default(), tmp.path(), tmp.path())
            .expect_err("a bare directory has no origin");
        assert!(err.contains("no git origin identity"), "{err}");
    }
}
