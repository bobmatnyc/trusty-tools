//! Getting the recipient's repositories onto their own disk.
//!
//! Why: every existing tga command requires each entry in `Config.repositories[]`
//! to already be a checkout on disk (`crates/trusty-git-analytics/src/core/config/mod.rs:325-340`),
//! and #5215 records that nothing anywhere clones — "zero `Repository::clone`
//! production hits". So an org-wide audit today needs a human to `git clone`
//! dozens of repositories by hand before the tool can run at all. This module is
//! that step.
//!
//! **Why `gh repo clone` and not `git`.** DOC-68 §8 decided the credential
//! question: cloning reuses the credential `gh auth login` already resolved,
//! through `gh`'s git-credential helper, rather than a second authentication
//! step. Two further facts settle the mechanism rather than merely permitting
//! it. `gh repo clone` configures that helper itself, so a private repository
//! clones with no token ever passing through this crate's hands or its argv.
//! And this workspace has no common entry point for spawning `git` —
//! `git grep 'Command::new("git")' -- crates/*/src` returns nothing — so
//! reaching for `git` directly would mean founding a second process-spawning
//! domain, which is exactly what CLAUDE.md's common-entry-point rule forbids
//! doing casually. `gh` already has one: `trusty_common::gh::GhCommand` (#5475).
//!
//! **What a caller may assume, and may not.** A directory under
//! [`Area::Repos`] is a COMPLETED clone, always. Work happens in a sibling
//! `<name>.partial` and is renamed into place only after `gh` exits zero, so an
//! interrupted run leaves a partial that the next run deletes and re-clones
//! rather than a half-checkout a later stage would silently analyze (#5215).
//!
//! **Partial failure does not abort the sequence** (DOC-68 §8, §14 Q2, extending
//! DOC-67 §9's continue-on-failure policy to the clone stage): one repository
//! failing to clone is named in [`CloneReport::gaps`] and the rest proceed. The
//! sequence aborts only when EVERY repository failed, which is the one case
//! where continuing would produce a report about nothing.
//!
//! Test: `super::clone_tests`, plus the `#[ignore]`d live clone there.

use std::path::{Path, PathBuf};

use trusty_common::gh::{GhCommand, GhError};

use crate::error::AuditError;
use crate::workdir::{Area, WorkDir};

/// Suffix of the in-progress directory a clone is built in.
pub const PARTIAL_SUFFIX: &str = ".partial";

/// Default ceiling on what the clones may occupy, in bytes (20 GiB).
///
/// #5215's closure condition is that disk use is "bounded/reported, not
/// unbounded". A default rather than `None` is the point: an org sweep the
/// recipient did not size in advance stops at a number, and the repositories it
/// did not reach are named as gaps instead of filling their disk.
pub const DEFAULT_BUDGET_BYTES: u64 = 20 * 1024 * 1024 * 1024;

/// How to clone.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CloneOptions {
    /// Fetch only the tip commit. Bounds disk use; loses history.
    pub shallow: bool,
    /// Stop once the clones occupy this many bytes. `None` is unbounded.
    pub budget_bytes: Option<u64>,
}

impl Default for CloneOptions {
    /// Shallow, and bounded at [`DEFAULT_BUDGET_BYTES`].
    ///
    /// Shallow is the default because tga's sweep is configured per repository
    /// and a hundred full clones is the case that fills a laptop. A caller that
    /// needs history sets `shallow: false` explicitly.
    fn default() -> Self {
        Self {
            shallow: true,
            budget_bytes: Some(DEFAULT_BUDGET_BYTES),
        }
    }
}

/// What happened to one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CloneState {
    /// Newly cloned by this run.
    Cloned,
    /// A completed clone was already there; nothing was fetched.
    Reused,
    /// The clone failed. Nothing was left under [`Area::Repos`] for it.
    Failed(String),
    /// Not attempted — the disk budget was already spent.
    Skipped(String),
}

impl CloneState {
    /// Is this a checkout a later stage may actually read?
    pub fn is_usable(&self) -> bool {
        matches!(self, CloneState::Cloned | CloneState::Reused)
    }
}

/// One repository's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ClonedRepo {
    /// `owner/name`, as given.
    pub name_with_owner: String,
    /// Where the checkout is, or would have been.
    pub path: PathBuf,
    /// What happened.
    pub state: CloneState,
    /// Bytes on disk. Zero unless the state is usable.
    pub bytes: u64,
}

/// The whole acquisition step's result.
///
/// Why: #5215 requires disk use to be reported, and DOC-68 §14 Q2 requires a
/// failed repository to be named the way an analysis gap is — so both are data
/// here rather than log lines, ready to go into `AuditManifest.report.gaps`.
/// What: per-repository outcomes, the total on disk, and one gap line per
/// repository that will not be in the audit.
/// Test: `super::clone_tests::a_failed_repo_becomes_a_gap_and_the_rest_proceed`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CloneReport {
    /// One entry per requested repository, in request order.
    pub repos: Vec<ClonedRepo>,
    /// Total bytes the usable checkouts occupy.
    pub total_bytes: u64,
    /// One line per repository excluded from the audit, and why.
    pub gaps: Vec<String>,
}

/// Reject anything that is not a plain `owner/name` before it becomes a path.
///
/// Why: the destination is built by joining these components under the working
/// directory, and `workdir`'s containment property — `rm -rf <root>` is a
/// complete uninstall — holds only while nothing this crate writes escapes the
/// root. `..`, an absolute path, or an embedded separator would each escape it.
/// An allowlist is used rather than a denylist because GitHub's own name charset
/// is narrow and a denylist has to anticipate every spelling of the same trick.
/// What: exactly two components, each non-empty and made only of ASCII
/// alphanumerics, `.`, `-`, `_`, and neither being `.` or `..`.
/// Test: `super::clone_tests::a_traversing_name_never_becomes_a_path`,
/// `super::clone_tests::every_destination_stays_inside_the_root`.
///
/// # Errors
///
/// [`AuditError::InvalidRepoName`] naming the rejected input.
pub fn destination(work: &WorkDir, name_with_owner: &str) -> Result<PathBuf, AuditError> {
    let reject = || AuditError::InvalidRepoName {
        name: name_with_owner.to_string(),
    };
    let (owner, name) = name_with_owner.split_once('/').ok_or_else(reject)?;
    for part in [owner, name] {
        if part.is_empty() || part == "." || part == ".." {
            return Err(reject());
        }
        if !part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Err(reject());
        }
    }
    Ok(work.path(Area::Repos).join(owner).join(name))
}

/// Refuse a `repos/` area that is not a real directory.
///
/// Why: `workdir.rs` states this debt explicitly — "repo cloning owes the same
/// check when it lands (#5215)". A symlink planted at `repos/` before the first
/// run sends every clone outside the root, where it survives the delete the
/// README promises is complete. `tools::install` already refuses the same shape
/// for `tools/` (#5495).
/// What: `symlink_metadata` on the area, so a symlink is seen as a symlink
/// rather than followed. An absent area is fine — [`WorkDir::create`] makes it.
/// Test: `super::clone_tests::a_symlinked_repos_area_is_refused`.
///
/// # Errors
///
/// [`AuditError::UnsafeArea`] naming the path and what is there instead.
fn ensure_repos_area_is_real(work: &WorkDir) -> Result<(), AuditError> {
    let path = work.path(Area::Repos);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(AuditError::WorkDir { path, source }),
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(meta) => Err(AuditError::UnsafeArea {
            path,
            kind: if meta.is_symlink() { "symlink" } else { "file" },
        }),
    }
}

/// The clone invocation, as a command rather than a run.
///
/// The `--` separates `gh`'s own flags from the ones it forwards to `git`.
/// Test: `super::clone_tests::a_shallow_clone_forwards_depth_to_git`.
fn clone_command(name_with_owner: &str, into: &Path, shallow: bool) -> GhCommand {
    let mut args: Vec<std::ffi::OsString> = vec![
        "repo".into(),
        "clone".into(),
        name_with_owner.into(),
        into.as_os_str().to_os_string(),
    ];
    if shallow {
        args.push("--".into());
        args.push("--depth=1".into());
    }
    // #5215: `GH_REPO` would otherwise override the repository argument.
    GhCommand::new(args).env_remove("GH_REPO")
}

/// Bytes occupied by a directory tree, not following symlinks.
fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|e| match e.path() {
            p if p.is_symlink() => 0,
            p if p.is_dir() => dir_size(&p),
            p => std::fs::symlink_metadata(&p).map_or(0, |m| m.len()),
        })
        .sum()
}

/// Turn one `gh` result into a state, moving the partial into place or removing it.
///
/// Why: this is the fail-open site. `gh repo clone` failing part-way leaves a
/// directory that LOOKS like a checkout, and a caller that reported success on
/// it would hand a half-fetched repository to the sweep, which would analyze it
/// and report on it as if it were whole. The rename is what makes "a directory
/// under `repos/` is a completed clone" true by construction rather than by
/// convention.
/// What: on `Ok`, renames `partial` onto `dest` and measures it; on `Err`,
/// removes `partial` and returns the reason. Never leaves `partial` behind.
/// Test: `super::clone_tests::a_failed_clone_leaves_nothing_behind`,
/// `super::clone_tests::a_successful_clone_is_renamed_into_place`.
fn finish_one(dest: &Path, partial: &Path, outcome: Result<(), GhError>) -> (CloneState, u64) {
    if let Err(source) = outcome {
        let _ = std::fs::remove_dir_all(partial);
        return (CloneState::Failed(source.to_string()), 0);
    }
    if let Err(source) = std::fs::rename(partial, dest) {
        let _ = std::fs::remove_dir_all(partial);
        return (
            CloneState::Failed(format!(
                "clone completed but could not be moved into place: {source}"
            )),
            0,
        );
    }
    let bytes = dir_size(dest);
    (CloneState::Cloned, bytes)
}

/// Assemble the report, deciding whether the sequence may continue.
///
/// Why: DOC-68 §14 Q2's decision, encoded rather than cited — one failure is a
/// gap and the run continues; every failure means there is nothing to audit and
/// the run refuses instead of producing a report about no repositories.
/// What: sums usable bytes, writes one gap line per non-usable repository, and
/// returns [`AuditError::AllClonesFailed`] when a non-empty request produced no
/// usable checkout at all.
/// Test: `super::clone_tests::a_failed_repo_becomes_a_gap_and_the_rest_proceed`,
/// `super::clone_tests::every_repo_failing_aborts_the_sequence`.
fn summarize(repos: Vec<ClonedRepo>) -> Result<CloneReport, AuditError> {
    let usable = repos.iter().filter(|r| r.state.is_usable()).count();
    if !repos.is_empty() && usable == 0 {
        return Err(AuditError::AllClonesFailed {
            attempted: repos.len(),
        });
    }
    let total_bytes = repos
        .iter()
        .filter(|r| r.state.is_usable())
        .map(|r| r.bytes)
        .sum();
    let gaps = repos
        .iter()
        .filter_map(|r| match &r.state {
            CloneState::Failed(why) => Some(format!(
                "{} was not audited — the clone failed: {why}",
                r.name_with_owner
            )),
            CloneState::Skipped(why) => {
                Some(format!("{} was not audited — {why}", r.name_with_owner))
            }
            _ => None,
        })
        .collect();
    Ok(CloneReport {
        repos,
        total_bytes,
        gaps,
    })
}

/// Clone every requested repository into the working directory's `repos/` area.
///
/// Why: #5215 — tga must be able to take a repository it has never seen and
/// produce a local checkout with no prior manual `git clone`.
/// What: validates the area and every name first, then per repository: reuses a
/// completed checkout, discards any leftover partial, clones into a fresh
/// partial, and renames it into place. Stops attempting new clones once the
/// budget is spent; failures become gaps.
/// Test: `super::clone_tests`, and `cloning_a_real_repository` (`#[ignore]`).
///
/// # Errors
///
/// [`AuditError::UnsafeArea`] for a `repos/` area that is not a real directory,
/// [`AuditError::InvalidRepoName`] for anything that is not a plain
/// `owner/name`, [`AuditError::WorkDir`] for a directory that cannot be made,
/// and [`AuditError::AllClonesFailed`] when nothing at all could be cloned.
pub async fn clone_all(
    work: &WorkDir,
    repos: &[String],
    options: &CloneOptions,
) -> Result<CloneReport, AuditError> {
    work.create()?;
    ensure_repos_area_is_real(work)?;

    // #5215: every name is validated before ANY clone runs, so a typo in the
    // last entry cannot leave the first ten half-acquired.
    let planned: Vec<(String, PathBuf)> = repos
        .iter()
        .map(|r| destination(work, r).map(|d| (r.clone(), d)))
        .collect::<Result<_, _>>()?;

    let mut out = Vec::with_capacity(planned.len());
    let mut spent: u64 = 0;
    for (name_with_owner, dest) in planned {
        if dest.is_dir() {
            let bytes = dir_size(&dest);
            spent += bytes;
            out.push(ClonedRepo {
                name_with_owner,
                path: dest,
                state: CloneState::Reused,
                bytes,
            });
            continue;
        }
        if options.budget_bytes.is_some_and(|b| spent >= b) {
            out.push(ClonedRepo {
                name_with_owner,
                path: dest,
                state: CloneState::Skipped(format!(
                    "the {spent}-byte disk budget for clones was already spent"
                )),
                bytes: 0,
            });
            continue;
        }

        let partial = partial_path(&dest);
        // A leftover partial is an interrupted previous run: discard and refetch
        // rather than resume, which is what keeps a corrupt tree from surviving.
        let _ = std::fs::remove_dir_all(&partial);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|source| AuditError::WorkDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let ran = clone_command(&name_with_owner, &partial, options.shallow)
            .output()
            .await
            .and_then(|o| o.ok())
            .map(|_| ());
        let (state, bytes) = finish_one(&dest, &partial, ran);
        spent += bytes;
        out.push(ClonedRepo {
            name_with_owner,
            path: dest,
            state,
            bytes,
        });
    }
    summarize(out)
}

/// The in-progress sibling of a destination.
fn partial_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(PARTIAL_SUFFIX);
    PathBuf::from(name)
}

#[cfg(test)]
mod clone_tests {
    use super::*;

    fn work_in(dir: &Path) -> WorkDir {
        let work = WorkDir::new(dir.join("work"));
        work.create().expect("create");
        work
    }

    fn gh_failure() -> GhError {
        GhError::NonZero {
            args: "repo clone acme/api".to_string(),
            status: "exit 1".to_string(),
            stderr: "could not read from remote repository".to_string(),
        }
    }

    fn cloned(name: &str, state: CloneState, bytes: u64) -> ClonedRepo {
        ClonedRepo {
            name_with_owner: name.to_string(),
            path: PathBuf::from("/work/repos").join(name),
            state,
            bytes,
        }
    }

    /// The property `workdir::layout_tests::every_layout_path_is_inside_the_root`
    /// proves for the layout, held for a caller-supplied repository name.
    #[test]
    fn every_destination_stays_inside_the_root() {
        let work = WorkDir::new("/engagement/work");
        for name in ["acme/api", "a/b", "Org-1/repo.name_2"] {
            let dest = destination(&work, name).expect("a plain name resolves");
            assert!(
                dest.starts_with(work.path(Area::Repos)),
                "{name} escaped: {}",
                dest.display()
            );
        }
    }

    #[test]
    fn a_traversing_name_never_becomes_a_path() {
        let work = WorkDir::new("/engagement/work");
        for name in [
            "../../etc",
            "acme/../../etc",
            "/absolute/path",
            "acme/sub/dir",
            "acme/",
            "/name",
            "acme",
            "acme/..",
            "./x",
            "acme/na me",
        ] {
            let err = destination(&work, name).expect_err("{name} must be refused");
            assert!(
                matches!(err, AuditError::InvalidRepoName { .. }),
                "{name}: {err:?}"
            );
        }
    }

    #[test]
    fn a_symlinked_repos_area_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = WorkDir::new(tmp.path().join("work"));
        std::fs::create_dir_all(work.root()).expect("root");
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("target");
        std::os::unix::fs::symlink(&elsewhere, work.path(Area::Repos)).expect("symlink");

        let err = ensure_repos_area_is_real(&work).expect_err("a symlinked area must be refused");
        let AuditError::UnsafeArea { kind, .. } = &err else {
            panic!("expected UnsafeArea, got {err:?}");
        };
        assert_eq!(*kind, "symlink");
    }

    #[test]
    fn a_real_repos_area_passes_the_guard() {
        let tmp = tempfile::tempdir().expect("tempdir");
        ensure_repos_area_is_real(&work_in(tmp.path())).expect("a real directory is fine");
    }

    #[test]
    fn a_shallow_clone_forwards_depth_to_git() {
        let argv = clone_command("acme/api", Path::new("/w/repos/acme/api"), true).argv_display();
        assert_eq!(argv, "repo clone acme/api /w/repos/acme/api -- --depth=1");
        let full = clone_command("acme/api", Path::new("/w/repos/acme/api"), false).argv_display();
        assert!(!full.contains("--depth"), "{full}");
    }

    /// The fail-open regression: a clone that died part-way must not leave a
    /// directory the sweep would later read as a whole repository.
    #[test]
    fn a_failed_clone_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dest = tmp.path().join("api");
        let partial = partial_path(&dest);
        std::fs::create_dir_all(partial.join(".git")).expect("half-fetched tree");
        std::fs::write(partial.join("README.md"), b"partial").expect("write");

        let (state, bytes) = finish_one(&dest, &partial, Err(gh_failure()));
        assert!(matches!(state, CloneState::Failed(_)), "{state:?}");
        assert_eq!(bytes, 0);
        assert!(
            !dest.exists(),
            "a failed clone must not appear as a checkout"
        );
        assert!(!partial.exists(), "the partial must be removed");
    }

    #[test]
    fn a_successful_clone_is_renamed_into_place() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dest = tmp.path().join("api");
        let partial = partial_path(&dest);
        std::fs::create_dir_all(&partial).expect("mkdir");
        std::fs::write(partial.join("f"), b"0123456789").expect("write");

        let (state, bytes) = finish_one(&dest, &partial, Ok(()));
        assert_eq!(state, CloneState::Cloned);
        assert_eq!(bytes, 10);
        assert!(dest.is_dir());
        assert!(!partial.exists());
    }

    #[test]
    fn a_failed_repo_becomes_a_gap_and_the_rest_proceed() {
        let report = summarize(vec![
            cloned("acme/api", CloneState::Cloned, 100),
            cloned("acme/web", CloneState::Failed("no such repo".into()), 0),
            cloned("acme/lib", CloneState::Reused, 50),
        ])
        .expect("one failure does not abort the sequence");
        assert_eq!(report.total_bytes, 150);
        assert_eq!(report.gaps.len(), 1);
        assert!(report.gaps[0].contains("acme/web"), "{:?}", report.gaps);
        assert!(report.gaps[0].contains("no such repo"), "{:?}", report.gaps);
    }

    #[test]
    fn every_repo_failing_aborts_the_sequence() {
        let err = summarize(vec![
            cloned("acme/api", CloneState::Failed("x".into()), 0),
            cloned("acme/web", CloneState::Failed("y".into()), 0),
        ])
        .expect_err("nothing cloned means nothing to audit");
        let AuditError::AllClonesFailed { attempted } = err else {
            panic!("expected AllClonesFailed, got {err:?}");
        };
        assert_eq!(attempted, 2);
    }

    #[test]
    fn asking_for_no_repositories_is_not_a_failure() {
        let report = summarize(Vec::new()).expect("an empty request is empty, not failed");
        assert!(report.repos.is_empty());
        assert_eq!(report.total_bytes, 0);
    }

    #[test]
    fn a_skipped_repo_is_named_as_a_gap_too() {
        let report = summarize(vec![
            cloned("acme/api", CloneState::Cloned, 10),
            cloned("acme/web", CloneState::Skipped("budget spent".into()), 0),
        ])
        .expect("a budget stop is not an abort");
        assert_eq!(report.gaps.len(), 1);
        assert!(report.gaps[0].contains("budget spent"), "{:?}", report.gaps);
    }

    #[tokio::test]
    async fn an_existing_checkout_is_reused_rather_than_refetched() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let dest = destination(&work, "acme/api").expect("valid name");
        std::fs::create_dir_all(&dest).expect("mkdir");
        std::fs::write(dest.join("f"), b"1234").expect("write");

        let report = clone_all(&work, &["acme/api".to_string()], &CloneOptions::default())
            .await
            .expect("a present checkout needs no network");
        assert_eq!(report.repos[0].state, CloneState::Reused);
        assert_eq!(report.total_bytes, 4);
    }

    #[tokio::test]
    async fn a_bad_name_is_refused_before_any_clone_runs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let err = clone_all(
            &work,
            &["acme/api".to_string(), "../escape".to_string()],
            &CloneOptions::default(),
        )
        .await
        .expect_err("the whole request is refused");
        assert!(matches!(err, AuditError::InvalidRepoName { .. }), "{err:?}");
        assert!(
            !work.path(Area::Repos).join("acme").exists(),
            "nothing may be acquired when the request is refused"
        );
    }

    /// A budget already spent stops further clones without touching the network.
    #[tokio::test]
    async fn a_spent_budget_skips_rather_than_clones() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let present = destination(&work, "acme/api").expect("valid");
        std::fs::create_dir_all(&present).expect("mkdir");
        std::fs::write(present.join("f"), b"12345678").expect("write");

        let report = clone_all(
            &work,
            &["acme/api".to_string(), "acme/web".to_string()],
            &CloneOptions {
                shallow: true,
                budget_bytes: Some(4),
            },
        )
        .await
        .expect("the first repo is usable, so the run continues");
        assert_eq!(report.repos[0].state, CloneState::Reused);
        assert!(
            matches!(report.repos[1].state, CloneState::Skipped(_)),
            "{:?}",
            report.repos[1].state
        );
        assert_eq!(report.gaps.len(), 1);
    }

    /// The whole path against a real remote.
    ///
    /// `#[ignore]` because it needs an authenticated `gh` and network —
    /// `cargo test -p trusty-audit -- --include-ignored` runs it.
    #[tokio::test]
    #[ignore = "clones over the network with a real `gh`; run with --include-ignored"]
    async fn cloning_a_real_repository() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let report = clone_all(
            &work,
            &["octocat/Hello-World".to_string()],
            &CloneOptions::default(),
        )
        .await
        .expect("a public repository clones");
        assert_eq!(report.repos[0].state, CloneState::Cloned);
        assert!(report.repos[0].path.join(".git").is_dir());
        assert!(report.total_bytes > 0, "the report must state disk use");
    }
}
