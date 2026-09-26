//! Which `CARGO_TARGET_DIR` a leased build gets (#8261).
//!
//! Why: the slot index is only half of a slot; the other half is a target
//! directory no concurrent build shares, so cargo's own build-directory lock
//! never serialises two leased builds and one build's artifacts never replace
//! another's. Holding flock `K` for the build's whole life is what makes pool
//! directory `slot-K` exclusive — the 45-minute daemon TTL that let a second
//! builder be handed a live build's directory is gone.
//!
//! What: [`choose`] is the pure rule. Only the machine's SHARED target
//! directory — the one `tm doctor`'s `rust_build_env` row exports, and which a
//! project `.envrc` commonly exports for every shell — is replaced by the held
//! slot's directory: it is the one directory concurrent worktrees share. An
//! unset `CARGO_TARGET_DIR` (cargo's `./target`, or a repo's
//! `build.target-dir`) and any other pinned directory are left alone. [`resolve_pool`] finds the pool for a checkout exactly as
//! the retired dispatch-time grant did.
//! Test: the `#[cfg(test)]` suite below.

use std::path::{Path, PathBuf};

use crate::core::builder_slot_pool::SlotPool;
use crate::core::builders::BuildersConfig;

/// What [`choose`] decided.
///
/// Test: every test below.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TargetDirChoice {
    /// The caller pinned this directory; leave it.
    Keep(String),
    /// Use this slot directory.
    Slot(PathBuf),
    /// No pool is available; the environment is left as it is, for this reason.
    Unchanged(String),
}

/// Decide the target directory for a leased build.
///
/// What: `ambient` is `CARGO_TARGET_DIR` as the build would inherit it;
/// `shared` is the machine's shared target directory; `slot` is the pool
/// directory for the held slot, or why there is none.
/// Test: `an_explicit_target_dir_is_kept`, `the_shared_dir_is_replaced_by_the_slot`,
/// `no_pool_leaves_the_environment_alone`, `an_unset_target_dir_is_left_to_cargo`,
/// `a_repo_pinned_target_dir_is_left_alone`.
#[must_use]
pub fn choose(
    ambient: Option<&str>,
    shared: Option<&Path>,
    slot: Result<PathBuf, String>,
) -> TargetDirChoice {
    // #8261 critic round 1 (PM decision): redirect ONLY the machine's shared
    // directory — the one case that shares state across worktrees. Unset
    // means cargo's own `./target` or a repo's `build.target-dir`, and a
    // binary a user runs from there must not go stale; that is left alone.
    let Some(dir) = ambient.filter(|d| !d.trim().is_empty()) else {
        return TargetDirChoice::Unchanged(
            "CARGO_TARGET_DIR is unset, so cargo's own target directory is kept".to_string(),
        );
    };
    if !shared.is_some_and(|s| same_path(Path::new(dir), s)) {
        return TargetDirChoice::Keep(dir.to_string());
    }
    match slot {
        Ok(path) => TargetDirChoice::Slot(path),
        Err(why) => TargetDirChoice::Unchanged(why),
    }
}

/// Paths equal after dropping trailing separators and `.` components.
fn same_path(a: &Path, b: &Path) -> bool {
    a.components().eq(b.components())
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
    let build = trusty_common::crate_config::load_at::<
        crate::core::trusty_tools_config::TrustyToolsConfig,
    >(&trusty_common::crate_config::crate_config_path_at(
        home,
        crate::core::trusty_tools_config::CRATE_NAME,
    ))
    .ok()
    .flatten();
    let shared = crate::core::build_env::resolve_build_env(
        build.as_ref().and_then(|c| c.build.as_ref()),
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

    #[test]
    fn an_explicit_target_dir_is_kept() {
        let got = choose(
            Some("/work/target-mine"),
            Some(Path::new("/shared/target")),
            Ok(PathBuf::from("/pool/slot-1")),
        );
        assert_eq!(got, TargetDirChoice::Keep("/work/target-mine".into()));
    }

    /// The index-recycling fix: the shared directory (as a project `.envrc`
    /// exports it) is replaced by the held slot's own directory.
    /// The index-recycling fix: the shared directory (as a project `.envrc`
    /// exports it) is replaced by the held slot's own directory.
    #[test]
    fn the_shared_dir_is_replaced_by_the_slot() {
        let slot = PathBuf::from("/pool/o/r/slot-1");
        for ambient in ["/shared/target", "/shared/target/"] {
            let got = choose(
                Some(ambient),
                Some(Path::new("/shared/target")),
                Ok(slot.clone()),
            );
            assert_eq!(got, TargetDirChoice::Slot(slot.clone()), "{ambient:?}");
        }
    }

    /// Critic round 1 (HIGH 3): unset means cargo's own `./target` (or the
    /// repo's `build.target-dir`); it is never redirected.
    #[test]
    fn an_unset_target_dir_is_left_to_cargo() {
        let got = choose(
            None,
            Some(Path::new("/shared/target")),
            Ok("/pool/slot-0".into()),
        );
        assert!(matches!(got, TargetDirChoice::Unchanged(_)), "{got:?}");
    }

    #[test]
    fn a_repo_pinned_target_dir_is_left_alone() {
        let got = choose(
            Some("/repo/custom-target"),
            Some(Path::new("/shared/target")),
            Ok("/pool/slot-0".into()),
        );
        assert_eq!(got, TargetDirChoice::Keep("/repo/custom-target".into()));
    }

    #[test]
    fn no_pool_leaves_the_environment_alone() {
        let got = choose(
            Some("/shared/target"),
            Some(Path::new("/shared/target")),
            Err("no origin".into()),
        );
        assert_eq!(got, TargetDirChoice::Unchanged("no origin".into()));
    }

    #[test]
    fn a_checkout_without_an_origin_has_no_pool() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = resolve_pool(&BuildersConfig::default(), tmp.path(), tmp.path())
            .expect_err("a bare directory has no origin");
        assert!(err.contains("no git origin identity"), "{err}");
    }
}
