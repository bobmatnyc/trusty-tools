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
/// What: `ambient` is `CARGO_TARGET_DIR` as the build would inherit it — the
/// caller passes an explicit `--target-dir` argv value here instead when the
/// command carries one ([`explicit_target_dir_arg`]), since that is the value
/// cargo will actually use; `pool` is the repo's slot pool and warm
/// directory, or why there is none.
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

/// The `--target-dir` value `argv` sets explicitly, if any (#8261 round 6).
///
/// Why: cargo's own `--target-dir` flag outranks `CARGO_TARGET_DIR` from the
/// environment, so a leased build whose argv carries `--target-dir <shared>`
/// kept writing into the shared directory even after [`plan`] replaced the
/// env var — [`plan`] never saw the flag, only `ambient`, which is the env
/// var alone (critic LOW). Reading the SAME value cargo itself resolves to is
/// what lets [`plan`] decide against the directory the build will actually
/// use.
/// What: the value of a separated `--target-dir <value>` or joined
/// `--target-dir=<value>` argument, whichever appears LAST — cargo itself
/// takes the last repeated flag. `None` when `argv` carries no such argument.
/// Scanning stops at the first literal `--`: everything after it belongs to
/// the test binary (or other forwarded program), not to cargo, so a
/// `--target-dir` appearing there is never cargo's own flag (#8261 round 7,
/// critic CRITICAL).
/// Test: `an_explicit_target_dir_flag_is_read_from_argv`,
/// `an_explicit_target_dir_flag_stops_at_the_double_dash`.
#[must_use]
pub fn explicit_target_dir_arg(argv: &[String]) -> Option<&str> {
    let mut found = None;
    let mut i = 0;
    while i < argv.len() {
        let tok = argv[i].as_str();
        if tok == "--" {
            break;
        }
        if tok == "--target-dir" {
            if let Some(value) = argv.get(i + 1) {
                found = Some(value.as_str());
            }
            i += 2;
            continue;
        }
        if let Some(value) = tok.strip_prefix("--target-dir=") {
            found = Some(value);
        }
        i += 1;
    }
    found
}

/// Replace an explicit `--target-dir` argument in `argv` with `new_dir`.
///
/// Why: setting `CARGO_TARGET_DIR` alone is not enough once argv carries its
/// own `--target-dir` — that flag still wins, so the leased slot directory
/// must land in the SAME argument cargo will read (#8261 round 6).
/// What: rewrites the LAST separated or joined `--target-dir` argument found
/// by [`explicit_target_dir_arg`] to `new_dir`, leaving every other argument
/// byte-identical. A no-op — returns `argv` unchanged — when it carries no
/// such flag. Scanning stops at the first literal `--`, same rule and same
/// reason as [`explicit_target_dir_arg`]: every argument at or after it is
/// the test binary's, never cargo's, and is left untouched (#8261 round 7,
/// critic CRITICAL).
/// Test: `an_explicit_target_dir_flag_is_rewritten_to_the_slot`,
/// `rewriting_the_target_dir_flag_leaves_everything_after_double_dash_untouched`.
#[must_use]
pub fn rewrite_target_dir_arg(argv: &[String], new_dir: &str) -> Vec<String> {
    let mut out = argv.to_vec();
    let mut separated: Option<usize> = None;
    let mut joined: Option<usize> = None;
    let mut i = 0;
    while i < out.len() {
        if out[i] == "--" {
            break;
        }
        if out[i] == "--target-dir" && i + 1 < out.len() {
            separated = Some(i + 1);
            joined = None;
            i += 2;
            continue;
        }
        if out[i].starts_with("--target-dir=") {
            joined = Some(i);
            separated = None;
        }
        i += 1;
    }
    if let Some(idx) = separated {
        out[idx] = new_dir.to_string();
    } else if let Some(idx) = joined {
        out[idx] = format!("--target-dir={new_dir}");
    }
    out
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
    // #8794: the pool's seeding ceiling is the lease's own slot count, since
    // a lease only ever holds slot indexes below it.
    let ceiling = config.effective_max_concurrent(crate::core::builders::host_memory_tier());
    Ok((
        SlotPool::new(config.effective_slot_pool_root(home), identity, ceiling),
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
        Ok((SlotPool::new(PathBuf::from("/pool"), id, 4), None))
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

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(ToString::to_string).collect()
    }

    /// #8261 round 6 (critic LOW): a separated, a joined, and a repeated flag
    /// (cargo takes the LAST one) — plus no flag at all.
    #[test]
    fn an_explicit_target_dir_flag_is_read_from_argv() {
        assert_eq!(
            explicit_target_dir_arg(&argv(&["cargo", "build", "--target-dir", "/shared"])),
            Some("/shared")
        );
        assert_eq!(
            explicit_target_dir_arg(&argv(&["cargo", "build", "--target-dir=/shared"])),
            Some("/shared")
        );
        assert_eq!(
            explicit_target_dir_arg(&argv(&[
                "cargo",
                "build",
                "--target-dir",
                "/first",
                "--target-dir",
                "/second"
            ])),
            Some("/second")
        );
        assert_eq!(
            explicit_target_dir_arg(&argv(&["cargo", "build", "-p", "x"])),
            None
        );
    }

    /// #8261 round 6 (critic LOW): the flag cargo will actually read is the
    /// one that must carry the slot, not just the env var.
    #[test]
    fn an_explicit_target_dir_flag_is_rewritten_to_the_slot() {
        assert_eq!(
            rewrite_target_dir_arg(
                &argv(&["cargo", "build", "--target-dir", "/shared"]),
                "/pool/slot-0"
            ),
            argv(&["cargo", "build", "--target-dir", "/pool/slot-0"])
        );
        assert_eq!(
            rewrite_target_dir_arg(
                &argv(&["cargo", "build", "--target-dir=/shared"]),
                "/pool/slot-0"
            ),
            argv(&["cargo", "build", "--target-dir=/pool/slot-0"])
        );
        // No flag: unchanged, byte for byte.
        assert_eq!(
            rewrite_target_dir_arg(&argv(&["cargo", "build", "-p", "x"]), "/pool/slot-0"),
            argv(&["cargo", "build", "-p", "x"])
        );
    }

    /// #8261 round 7 (critic CRITICAL): everything after a literal `--`
    /// belongs to the test binary, not cargo — a `--target-dir` there is
    /// never cargo's own flag, and one before it must still be found.
    #[test]
    fn an_explicit_target_dir_flag_stops_at_the_double_dash() {
        assert_eq!(
            explicit_target_dir_arg(&argv(&[
                "cargo",
                "test",
                "--target-dir",
                "/shared",
                "--",
                "--target-dir",
                "x"
            ])),
            Some("/shared")
        );
        assert_eq!(
            explicit_target_dir_arg(&argv(&["cargo", "test", "--", "--target-dir", "X"])),
            None
        );
    }

    /// #8261 round 7 (critic CRITICAL): rewriting the pre-`--` flag must
    /// leave every post-`--` argument byte-identical to the input.
    #[test]
    fn rewriting_the_target_dir_flag_leaves_everything_after_double_dash_untouched() {
        let input = argv(&[
            "cargo",
            "test",
            "--target-dir",
            "/shared",
            "--",
            "--target-dir",
            "x",
        ]);
        assert_eq!(
            rewrite_target_dir_arg(&input, "/pool/slot-0"),
            argv(&[
                "cargo",
                "test",
                "--target-dir",
                "/pool/slot-0",
                "--",
                "--target-dir",
                "x",
            ])
        );
        // No flag before `--`: the whole argv, including the part after
        // `--`, is unchanged, byte for byte.
        let no_pre_dash_flag = argv(&["cargo", "test", "--", "--target-dir", "X"]);
        assert_eq!(
            rewrite_target_dir_arg(&no_pre_dash_flag, "/pool/slot-0"),
            no_pre_dash_flag
        );
    }
}
