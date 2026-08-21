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
//! And `gh` has a common entry point in this workspace —
//! `trusty_common::gh::GhCommand` (#5475) — while `git` has none, so reaching
//! for `git` where `gh` answers would be a second implementation of one
//! capability. That reasoning holds for a REMOTE and stops there: `gh repo
//! clone` takes an `owner/repo` and cannot address a path, so #6001's local
//! source spawns `git` in the one function [`crate::local_repo`] keeps it in.
//! `trusty-audit` is a leaf crate and that spawn is not shared with any other,
//! so consolidating it is a question for whoever founds a common `git` entry
//! point, not a reason to leave a 1.4 GB checkout unreadable.
//!
//! **What a caller may assume, and may not.** A directory under
//! [`Area::Repos`] is a COMPLETED, VERIFIED checkout, always. Work happens
//! under [`Area::State`] (see [`STAGING_DIR`]) and is renamed into place only
//! after `gh` exits zero AND [`verify_checkout`] confirms a resolvable `HEAD`,
//! so neither an interrupted run nor a commitless repository leaves something a
//! later stage would silently analyze as whole (#5215).
//!
//! **Partial failure does not abort the sequence** (DOC-68 §8, §14 Q2, extending
//! DOC-67 §9's continue-on-failure policy to the clone stage): one repository
//! failing to clone is named in [`CloneReport::gaps`] and the rest proceed. The
//! sequence aborts only when EVERY repository failed, which is the one case
//! where continuing would produce a report about nothing.
//!
//! **Two kinds of source, one downstream** (#6001). An entry in the request is
//! either a GitHub `owner/repo` or an ABSOLUTE path to a checkout already on
//! disk — [`crate::local_repo::is_local_spec`] owns that decision, and
//! [`crate::local_repo`] owns the invariant that the operator's checkout is
//! never modified. The fork lives in [`resolve`] and [`acquire`] and nowhere
//! else: a local source is acquired into staging, verified and promoted by the
//! same three steps, so nothing past this module can tell the two apart.
//!
//! **Acquiring is selecting** (#5556). What lands here is what the sweep audits:
//! [`clone_all`] records the usable checkouts in `state/`[`run::SELECTION_FILE`]
//! so `taudit clone …` and `taudit run` are one chain rather than two stages
//! with an unwritten file between them.
//!
//! Test: `super::clone_tests`, `tests/cli_end_to_end.rs`, plus the `#[ignore]`d
//! live clone there.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use trusty_common::gh::GhCommand;

use crate::error::AuditError;
use crate::local_repo;
use crate::progress::{Operation, Progress, UnitOutcome};
use crate::run::{self, SelectedRepo};
use crate::workdir::{Area, WorkDir};

/// Directory under [`Area::State`] where in-progress clones are built.
///
/// Why: the staging path must be one no repository name can address. Building
/// `<dest>.partial` beside the destination was not: `.` is in the name
/// allowlist, so the legal repository `acme/api.partial` resolved to exactly
/// the staging path of `acme/api`, and cloning the latter deleted the former's
/// completed checkout (#5215 review). Staging outside `repos/` entirely makes
/// the collision unrepresentable rather than merely unlikely — `repos/<owner>/<name>`
/// can never reach `state/clone-staging/`.
/// Test: `super::clone_tests::a_repo_named_like_the_old_staging_path_is_safe`.
pub const STAGING_DIR: &str = "clone-staging";

/// Default ceiling on what the clones may occupy, in bytes (20 GiB).
///
/// Read [`CloneOptions::budget_bytes`] for what this does and does not bound —
/// it stops new clones from STARTING, and does not cap one already running.
pub const DEFAULT_BUDGET_BYTES: u64 = 20 * 1024 * 1024 * 1024;

/// How to clone.
///
/// #5916: there is no depth knob. A shallow clone is what made the tga database
/// degenerate — see [`clone_command`] — and the disk it saves is what
/// [`CloneOptions::budget_bytes`] is for.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CloneOptions {
    /// Stop STARTING new clones once this much is already on disk. `None` never
    /// stops.
    ///
    /// This is a start gate, not a cap. It is checked between repositories and
    /// nothing interrupts a clone in flight, so a single repository larger than
    /// the remaining budget still lands in full — 19 GiB spent against a 20 GiB
    /// budget admits a 100 GB monorepo and finishes at 119 GiB. Capping one
    /// clone needs a watchdog that kills the child mid-fetch, which is not
    /// implemented (#5215 review). Say "stops starting", never "bounded".
    /// Test: `super::clone_tests::a_spent_budget_skips_rather_than_clones`.
    pub budget_bytes: Option<u64>,
}

impl Default for CloneOptions {
    /// Bounded at [`DEFAULT_BUDGET_BYTES`].
    fn default() -> Self {
        Self {
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
    /// `gh` exited zero but produced no checkout worth analyzing.
    ///
    /// Why: its own state rather than a flavour of `Failed`, because the cause
    /// is different and so is what the recipient should do. Cloning a
    /// commitless repository EXITS ZERO and leaves a directory containing
    /// only `.git`; `gh repo clone` forwards that status. Reported as `Cloned`,
    /// that is an audit claiming coverage of a repository it never read
    /// (#5215 review).
    /// Test: `super::clone_tests::a_commitless_clone_is_not_a_usable_checkout`.
    Empty(String),
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
    /// Whether [`ClonedRepo::bytes`] counted the whole tree.
    ///
    /// `false` when part of the walk was unreadable, which makes `bytes` a
    /// floor rather than a total. Reported instead of swallowed so the budget's
    /// arithmetic and the recipient's disk figure are not quietly confident
    /// about a number a failed walk produced (#5215 review).
    /// Test: `super::clone_tests::an_unreadable_subtree_marks_the_size_incomplete`.
    pub bytes_complete: bool,
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
    /// Whether [`CloneReport::total_bytes`] is a total or a floor.
    ///
    /// `false` when any repository's walk hit something unreadable — render it
    /// as "at least", never as the figure.
    pub total_bytes_complete: bool,
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
    let (owner, name) = split_name(name_with_owner)?;
    Ok(work.path(Area::Repos).join(owner).join(name))
}

/// Where one acquisition spec comes from, and what it is audited as.
///
/// Why: #6001 — an entry in the request is now either a GitHub `owner/repo` or
/// an absolute path to a checkout already on disk, and the fork has to be made
/// exactly once. Resolving it here, ahead of the loop, is what lets every name
/// be validated and every destination be checked for collisions before ANY
/// clone runs — the property `a_bad_name_is_refused_before_any_clone_runs`
/// already held for names.
/// Test: `super::clone_tests::a_local_path_is_acquired_under_the_local_owner`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// `gh repo clone <owner/name>`.
    Remote,
    /// `git clone <path>`, reading the operator's checkout and never writing it.
    Local(PathBuf),
}

/// What one request entry's GitHub-issue identity resolved to (#6130).
///
/// Why: the identity has to be decided where the SOURCE is still in hand, and
/// this is the only place it is — by the time `crate::run` reads the selection,
/// a local target is indistinguishable from a remote one, which is the whole
/// point of [`acquire`]. Resolving it here also keeps it fresh: a re-run reads
/// the remote again rather than trusting a value the registry recorded at `add`
/// time, possibly hours or a rename ago.
/// What: the `owner/repo` tga should query, or the sentence explaining why
/// there is none. Never "unknown" — every planned entry gets one or the other,
/// so `crate::run` never has to guess.
/// Test: `super::clone_tests::a_local_path_with_a_github_origin_records_the_real_slug`,
/// `super::clone_tests::a_local_path_with_no_github_origin_records_why`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GithubIdentity {
    /// GitHub issues for this entry live under this `owner/repo`.
    Slug(String),
    /// No GitHub identity, for this reason.
    None(String),
}

/// Resolve every planned entry's GitHub-issue identity.
///
/// A REMOTE entry is its own slug — the request named `owner/repo` and `gh`
/// cloned from it, so nothing needs reading. A LOCAL entry is whatever its
/// source checkout's `origin` remote addresses on github.com, which is the read
/// [`crate::local_repo::github_slug`] performs; a source that names no such
/// repository — no remote at all, or a remote pointing somewhere else — gets
/// the sentence the report prints instead of a slug the API would refuse.
///
/// Runs against the SOURCE, so it covers a reused checkout too: `clone_all`
/// skips acquisition when the destination already exists, and an identity
/// resolved only inside [`acquire`] would be missing for exactly those entries.
///
/// **A remote that was never READ gets a different sentence from one that was
/// read and named nothing** (#6130 review). `git` failing to spawn proves
/// nothing about the checkout's remotes, so claiming it "names no repository on
/// github.com" would put a fact in the report that nothing established. Both
/// are still declared absences the run continues past — the leg genuinely was
/// not attempted either way — but the reason says which happened.
async fn resolve_github_identities(
    planned: &[(String, Source, PathBuf, PathBuf)],
) -> BTreeMap<String, GithubIdentity> {
    let mut out = BTreeMap::new();
    for (name, source, _, _) in planned {
        let identity = match source {
            Source::Remote => GithubIdentity::Slug(name.clone()),
            Source::Local(path) => match local_repo::github_slug(path).await {
                Ok(Some(slug)) => GithubIdentity::Slug(slug),
                Ok(None) => GithubIdentity::None(format!(
                    "`{name}` was audited from the checkout at {}, whose `origin` remote names no \
                     repository on github.com — its issues could not be located, so no GitHub \
                     work-item collection was attempted",
                    path.display()
                )),
                Err(reason) => GithubIdentity::None(format!(
                    "`{name}` was audited from the checkout at {}, whose `origin` remote could \
                     not be read ({reason}) — its issues could not be located, so no GitHub \
                     work-item collection was attempted. This says nothing about whether the \
                     checkout has a GitHub remote.",
                    path.display()
                )),
            },
        };
        out.insert(name.clone(), identity);
    }
    out
}

/// Split one request entry into what it is audited as and where it comes from.
///
/// [`local_repo::is_local_spec`] owns the disambiguation, so `registry::parse`
/// and this cannot decide differently about the same string.
fn resolve(spec: &str) -> Result<(String, Source), AuditError> {
    if !local_repo::is_local_spec(spec) {
        return Ok((spec.to_owned(), Source::Remote));
    }
    let path = local_repo::normalize(spec);
    let name = local_repo::derive_name(&path)?;
    Ok((name, Source::Local(path)))
}

/// Refuse a request in which two entries would occupy one checkout directory.
///
/// Why: #6001. `clone_all` REUSES a directory that is already under `repos/`,
/// which is correct for a re-run and wrong for a collision — `/srv/a/apex` and
/// `/srv/b/apex` both derive `local/apex`, so the second would be reported as
/// audited having read the first one's history. Refusing the whole request is
/// the same shape as the name check beside it: nothing is acquired when the
/// request cannot be honoured as asked.
///
/// **The comparison is case-folded** ([`local_repo::case_fold`]): on a
/// case-insensitive, case-preserving filesystem — APFS's default, and the one
/// this feature runs on — `repos/local/Apex` and `repos/local/apex` are ONE
/// directory on disk even though [`derive_name`](local_repo::derive_name)
/// preserves case, so a plain string comparison of `dest` misses exactly the
/// collision this function exists to catch. Folding is unconditional, not
/// gated on detecting the filesystem's case sensitivity — a false-positive
/// refusal of a genuinely distinct pair on a case-sensitive filesystem is far
/// cheaper than a misattributed audit.
/// Test: `super::clone_tests::two_paths_with_one_basename_are_refused_together`,
/// `super::clone_tests::two_paths_that_differ_only_by_case_are_refused_together`.
fn refuse_collisions(planned: &[(String, Source, PathBuf, PathBuf)]) -> Result<(), AuditError> {
    for (index, (name, source, dest, _)) in planned.iter().enumerate() {
        let dest_fold = local_repo::case_fold(&dest.to_string_lossy());
        for (other_name, other_source, other_dest, _) in &planned[index + 1..] {
            if dest_fold != local_repo::case_fold(&other_dest.to_string_lossy()) {
                continue;
            }
            // The same repository listed twice is the idempotent case, not a
            // collision: it is one checkout either way.
            if source == other_source {
                continue;
            }
            return Err(AuditError::CollidingCheckouts {
                first: spec_of(name, source),
                second: spec_of(other_name, other_source),
                name: name.clone(),
            });
        }
    }
    Ok(())
}

/// What an operator typed for this entry, for a message that points at it.
fn spec_of(name: &str, source: &Source) -> String {
    match source {
        Source::Remote => name.to_owned(),
        Source::Local(path) => path.display().to_string(),
    }
}

/// Where a clone is built before it is renamed into place.
///
/// Under [`Area::State`], not beside the destination — see [`STAGING_DIR`] for
/// the collision that forced it. Same working-directory root as the
/// destination, so the rename stays a same-filesystem atomic move.
fn staging(work: &WorkDir, name_with_owner: &str) -> Result<PathBuf, AuditError> {
    let (owner, name) = split_name(name_with_owner)?;
    Ok(work
        .path(Area::State)
        .join(STAGING_DIR)
        .join(owner)
        .join(name))
}

/// Split and validate `owner/name`, the one place the charset is decided.
///
/// `pub(crate)` since #5822: registering a repository target validates the same
/// identity before persisting it, and a second charset decision there would let
/// `taudit add repo` accept a name `taudit clone` then refuses.
pub(crate) fn split_name(name_with_owner: &str) -> Result<(&str, &str), AuditError> {
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
    Ok((owner, name))
}

/// Refuse a path that exists but is not a real directory.
///
/// Why: `workdir.rs` states this debt explicitly — "repo cloning owes the same
/// check when it lands (#5215)". A symlink sends what is written through it
/// outside the root, where it survives the delete the README promises is
/// complete. `tools::install` already refuses the same shape for `tools/`
/// (#5495).
///
/// It is applied at THREE levels, because guarding only `repos/` left the two
/// below it open: a planted `repos/acme -> /Users/victim/.ssh` needs exactly
/// the same precondition as a planted `repos/`, and `create_dir_all` follows
/// it without complaint (#5215 review).
/// What: `symlink_metadata`, so a symlink is seen rather than followed. An
/// absent path is fine — it is about to be created.
/// Test: `super::clone_tests::a_symlinked_repos_area_is_refused`,
/// `super::clone_tests::a_symlinked_owner_directory_is_refused`.
///
/// # Errors
///
/// [`AuditError::UnsafeArea`] naming the path and what is there instead.
fn ensure_real_dir(path: PathBuf) -> Result<(), AuditError> {
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
/// Why (#5916): this used to append `-- --depth=1`, and that one flag emptied
/// the whole git-analytics leg. A depth-1 checkout has exactly one commit, so
/// tga collected `commits=1`, `authors=1`, a period whose start equals its end,
/// and every line in the tree attributed to whoever last touched it — measured
/// on `BurntSushi/xsv`, whose real history is 407 commits by 30 authors from
/// 2014 to 2025. Every CSV in the deliverable was a header and one row. What
/// bounds the disk that saved is [`CloneOptions::budget_bytes`], which stops
/// STARTING clones and does not need history thrown away to work: the same
/// repository is 628 KiB shallow and 1.2 MiB full, against a 20 GiB default.
/// What: `gh repo clone <owner/name> <dest>`, with no flags forwarded to `git`
/// at all — so there is no `--` separator either.
/// Test: `super::clone_tests::a_clone_asks_git_for_the_whole_history`.
fn clone_command(name_with_owner: &str, into: &Path) -> GhCommand {
    let args: Vec<std::ffi::OsString> = vec![
        "repo".into(),
        "clone".into(),
        name_with_owner.into(),
        into.as_os_str().to_os_string(),
    ];
    // #5215: `GH_REPO` would otherwise override the repository argument.
    GhCommand::new(args).env_remove("GH_REPO")
}

/// Bytes occupied by a directory tree, and whether the walk saw all of it.
///
/// Why: an unreadable subdirectory used to contribute 0 silently, so a failed
/// walk produced a confident-looking total that understated disk use and made
/// the budget stop later than asked. The flag is what stops the caller
/// presenting a floor as a figure (#5215 review).
/// What: recursive, never following symlinks. `false` means some entry could
/// not be read, so the count is a lower bound.
/// Test: `super::clone_tests::an_unreadable_subtree_marks_the_size_incomplete`.
fn dir_size(path: &Path) -> (u64, bool) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return (0, false);
    };
    let mut total = 0u64;
    let mut complete = true;
    for entry in entries {
        let Ok(entry) = entry else {
            complete = false;
            continue;
        };
        let child = entry.path();
        if child.is_symlink() {
            continue;
        }
        if child.is_dir() {
            let (bytes, ok) = dir_size(&child);
            total = total.saturating_add(bytes);
            complete &= ok;
        } else {
            match std::fs::symlink_metadata(&child) {
                Ok(meta) => total = total.saturating_add(meta.len()),
                Err(_) => complete = false,
            }
        }
    }
    (total, complete)
}

/// Is this staged tree a checkout the sweep can actually read?
///
/// Why: `gh` exiting zero is not proof of a usable repository. Cloning a
/// COMMITLESS repository exits zero and leaves a directory
/// holding only `.git`, and `gh repo clone` forwards that status — reported as
/// `Cloned`, the audit claims coverage of a repository nothing ever read
/// (#5215 review). This is the check the `#[ignore]`d live test was making and
/// the production path was not.
/// What: `.git` must be a real directory, and `HEAD` must resolve — either to a
/// detached SHA, or to a ref that exists loose or in `packed-refs`. An
/// unresolvable `HEAD` is exactly the commitless case.
/// Test: `super::clone_tests::a_commitless_clone_is_not_a_usable_checkout`,
/// `super::clone_tests::a_checkout_with_a_packed_head_ref_is_usable`.
fn verify_checkout(tree: &Path) -> Result<(), String> {
    let git = tree.join(".git");
    if !git.is_dir() {
        return Err("the clone left no .git directory".to_string());
    }
    let head = match std::fs::read_to_string(git.join("HEAD")) {
        Ok(text) => text.trim().to_string(),
        Err(source) => return Err(format!("the clone left no readable .git/HEAD: {source}")),
    };
    let Some(reference) = head.strip_prefix("ref:").map(str::trim) else {
        // A detached HEAD is a raw SHA, which means there is a commit.
        return Ok(());
    };
    if git.join(reference).exists() {
        return Ok(());
    }
    let packed = std::fs::read_to_string(git.join("packed-refs")).unwrap_or_default();
    if packed.lines().any(|line| line.ends_with(reference)) {
        return Ok(());
    }
    Err(format!(
        "the repository has no commits — HEAD points at {reference}, which does not exist"
    ))
}

/// Turn one `gh` result into a state, moving the partial into place or removing it.
///
/// Why: this is the fail-open site. `gh repo clone` failing part-way leaves a
/// directory that LOOKS like a checkout, and a caller that reported success on
/// it would hand a half-fetched repository to the sweep, which would analyze it
/// and report on it as if it were whole. The rename is what makes "a directory
/// under `repos/` is a completed clone" true by construction rather than by
/// convention.
/// What: on `Err`, removes the staged tree and returns the reason. On `Ok`,
/// VERIFIES the tree before promoting it — a zero exit that produced no usable
/// checkout becomes [`CloneState::Empty`], and nothing is promoted. Only a
/// verified tree is renamed onto `dest`. Never leaves the staged tree behind.
/// Test: `super::clone_tests::a_failed_clone_leaves_nothing_behind`,
/// `super::clone_tests::a_successful_clone_is_renamed_into_place`,
/// `super::clone_tests::a_commitless_clone_is_not_a_usable_checkout`.
fn finish_one(dest: &Path, staged: &Path, outcome: Result<(), String>) -> (CloneState, u64, bool) {
    let discard = |state: CloneState| {
        let _ = std::fs::remove_dir_all(staged);
        (state, 0, true)
    };
    // #6001: the reason arrives as a string rather than a `GhError`, because
    // acquisition now has two mechanisms and only one of them is `gh`.
    if let Err(reason) = outcome {
        return discard(CloneState::Failed(reason));
    }
    // #5215: verify BEFORE the rename, so an unusable tree never occupies the
    // destination even briefly.
    if let Err(why) = verify_checkout(staged) {
        return discard(CloneState::Empty(why));
    }
    if let Err(source) = std::fs::rename(staged, dest) {
        return discard(CloneState::Failed(format!(
            "clone completed but could not be moved into place: {source}"
        )));
    }
    let (bytes, complete) = dir_size(dest);
    (CloneState::Cloned, bytes, complete)
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
    let usable_repos = || repos.iter().filter(|r| r.state.is_usable());
    let total_bytes = usable_repos().map(|r| r.bytes).sum();
    let total_bytes_complete = usable_repos().all(|r| r.bytes_complete);
    let gaps = repos
        .iter()
        .filter_map(|r| match &r.state {
            CloneState::Failed(why) => Some(format!(
                "{} was not audited — the clone failed: {why}",
                r.name_with_owner
            )),
            CloneState::Empty(why) => Some(format!(
                "{} was not audited — nothing was cloned: {why}",
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
        total_bytes_complete,
        gaps,
    })
}

/// Record what was acquired as the selection the sweep will audit.
///
/// Why: #5556 — `taudit clone …` followed by `taudit run` was not a chain.
/// Nothing wrote `state/`[`run::SELECTION_FILE`], so the sweep refused with
/// "nothing to audit" over a working directory full of checkouts. On the command
/// line the clone invocation IS the selection, which is the sense in which
/// #5215 is one of the producers `run.rs` names.
///
/// Only USABLE checkouts go in. A repository that failed is already a gap, and
/// selecting it as well would fail it a second time for one cause — as a
/// missing checkout, in a sweep that had no way to know it was never acquired.
/// What: maps each usable [`ClonedRepo`] onto a [`SelectedRepo`] whose path is
/// relative to the working-directory root (the shape [`run::SELECTION_FILE`]
/// documents), and hands it to [`run::save_selection`], which owns the write.
/// An acquisition that produced nothing usable writes nothing, so a previous
/// selection survives a run in which every repository failed.
/// Test: `super::clone_tests::a_completed_clone_is_recorded_as_the_selection`,
/// `super::clone_tests::an_empty_request_leaves_an_existing_selection_alone`,
/// and `tests/cli_end_to_end.rs`.
fn record_selection(
    work: &WorkDir,
    report: &CloneReport,
    github: &BTreeMap<String, GithubIdentity>,
) -> Result<(), AuditError> {
    let selected: Vec<SelectedRepo> = report
        .repos
        .iter()
        .filter(|repo| repo.state.is_usable())
        .map(|repo| {
            let (slug, absent) = match github.get(&repo.name_with_owner) {
                Some(GithubIdentity::Slug(slug)) => (Some(slug.clone()), None),
                Some(GithubIdentity::None(reason)) => (None, Some(reason.clone())),
                // Unreachable in practice: every planned entry is resolved.
                // Left as neither, which `SelectedRepo::github_leg` resolves
                // from the name — the same answer a pre-#6130 file gets.
                None => (None, None),
            };
            SelectedRepo {
                name: repo.name_with_owner.clone(),
                path: repo
                    .path
                    .strip_prefix(work.root())
                    .unwrap_or(&repo.path)
                    .to_path_buf(),
                github_slug: slug,
                github_absent: absent,
            }
        })
        .collect();
    if selected.is_empty() {
        return Ok(());
    }
    run::save_selection(work, &selected)
}

/// Clone every requested repository into the working directory's `repos/` area.
///
/// Why: #5215 — tga must be able to take a repository it has never seen and
/// produce a local checkout with no prior manual `git clone`.
/// What: guards `repos/` BEFORE creating anything, validates every name, then
/// per repository: reuses a completed checkout, discards any leftover staged
/// tree, clones into staging, verifies it, and renames it into place. Stops
/// STARTING clones once the budget is spent; failures become gaps. Finally
/// records the usable checkouts as the sweep's selection — see
/// [`record_selection`].
/// Test: `super::clone_tests`, `tests/cli_end_to_end.rs`, and
/// `cloning_a_real_repository` (`#[ignore]`).
///
/// # Errors
///
/// [`AuditError::UnsafeArea`] for a `repos/` area, owner directory, or
/// destination that is not a real directory, [`AuditError::InvalidRepoName`]
/// for anything that is neither a plain `owner/name` nor an absolute path,
/// [`AuditError::LocalRepoUnusable`] for a path with no name a checkout can be
/// made under, [`AuditError::CollidingCheckouts`] when two entries would occupy
/// one destination, [`AuditError::WorkDir`] for a directory that cannot be made
/// or a selection that cannot be recorded, and [`AuditError::AllClonesFailed`]
/// when nothing at all could be cloned.
pub async fn clone_all(
    work: &WorkDir,
    repos: &[String],
    options: &CloneOptions,
    progress: &Progress,
) -> Result<CloneReport, AuditError> {
    // #5215 review: the guard runs BEFORE `create`. The other order let
    // `create_dir_all` follow a DANGLING `repos/` symlink and build the target
    // outside the root, so `UnsafeArea`'s "nothing was written" was false by
    // the time it was returned.
    ensure_real_dir(work.path(Area::Repos))?;
    work.create()?;

    // #5215: every name is validated before ANY clone runs, so a typo in the
    // last entry cannot leave the first ten half-acquired.
    // #6001: and every spec is resolved to its source in the same pass, so the
    // two shapes diverge exactly once.
    let planned: Vec<(String, Source, PathBuf, PathBuf)> = repos
        .iter()
        .map(|spec| {
            let (name, source) = resolve(spec)?;
            Ok((
                name.clone(),
                source,
                destination(work, &name)?,
                staging(work, &name)?,
            ))
        })
        .collect::<Result<_, AuditError>>()?;
    // #6001: two entries sharing a destination is refused here rather than
    // silently reused as one another's checkout.
    refuse_collisions(&planned)?;

    // #6130: decided here, where the source is still distinguishable, and
    // before the loop so a reused checkout gets the same answer a freshly
    // cloned one does.
    let github = resolve_github_identities(&planned).await;

    // #5823: announced only once every name has validated, so a display never
    // opens on an acquisition that a typo is about to refuse.
    let total = planned.len();
    progress.operation_started(Operation::CloneRepos, total);
    let mut out = Vec::with_capacity(total);
    let mut spent: u64 = 0;
    for (index, (name_with_owner, source, dest, staged)) in planned.into_iter().enumerate() {
        progress.unit_started(
            Operation::CloneRepos,
            name_with_owner.as_str(),
            index + 1,
            total,
        );
        // Each level between the area and the checkout is its own escape route.
        if let Some(owner_dir) = dest.parent() {
            ensure_real_dir(owner_dir.to_path_buf())?;
        }
        ensure_real_dir(dest.clone())?;

        if dest.is_dir() {
            let (bytes, complete) = dir_size(&dest);
            spent = spent.saturating_add(bytes);
            announce(progress, &name_with_owner, &CloneState::Reused);
            out.push(ClonedRepo {
                name_with_owner,
                path: dest,
                state: CloneState::Reused,
                bytes,
                bytes_complete: complete,
            });
            continue;
        }
        if options.budget_bytes.is_some_and(|b| spent >= b) {
            let state = CloneState::Skipped(format!(
                "the {spent}-byte disk budget for clones was already spent"
            ));
            announce(progress, &name_with_owner, &state);
            out.push(ClonedRepo {
                name_with_owner,
                path: dest,
                state,
                bytes: 0,
                bytes_complete: true,
            });
            continue;
        }

        // A leftover staged tree is an interrupted previous run: discard and
        // refetch rather than resume, which is what keeps a corrupt tree from
        // surviving.
        let _ = std::fs::remove_dir_all(&staged);
        for parent in [dest.parent(), staged.parent()].into_iter().flatten() {
            std::fs::create_dir_all(parent).map_err(|source| AuditError::WorkDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let ran = acquire(&name_with_owner, &source, &staged).await;
        let (state, bytes, complete) = finish_one(&dest, &staged, ran);
        spent = spent.saturating_add(bytes);
        announce(progress, &name_with_owner, &state);
        out.push(ClonedRepo {
            name_with_owner,
            path: dest,
            state,
            bytes,
            bytes_complete: complete,
        });
    }
    let usable = out.iter().filter(|r| r.state.is_usable()).count();
    progress.operation_finished(Operation::CloneRepos, usable, total);
    let report = summarize(out)?;
    record_selection(work, &report, &github)?;
    Ok(report)
}

/// Fetch one repository into `staged`, whichever kind of source it has.
///
/// Why: #6001 — the one place the two mechanisms differ. A LOCAL source is
/// re-inspected here even though registration already proved it: a sweep runs
/// hours after an `add`, and a source that has since been deleted, replaced
/// with a shallow clone, or truncated must become a named gap rather than a
/// thin report. That check is cheap (three `git rev-parse` reads) against a
/// clone that is not.
/// What: `gh repo clone` for a remote, `git clone <path>` for a local one.
/// Either failure comes back as the reason `finish_one` records, so a bad
/// source is one repository's gap and not the sweep's abort.
/// Test: `super::clone_tests::a_local_path_is_acquired_under_the_local_owner`,
/// `super::clone_tests::a_source_that_went_bad_after_registration_is_a_gap`.
async fn acquire(name_with_owner: &str, source: &Source, staged: &Path) -> Result<(), String> {
    match source {
        Source::Remote => clone_command(name_with_owner, staged)
            .output()
            .await
            .and_then(|o| o.ok())
            .map(|_| ())
            .map_err(|e| e.to_string()),
        Source::Local(path) => {
            local_repo::inspect(path)
                .await
                .map_err(|reason| format!("{} is no longer usable: {reason}", path.display()))?;
            local_repo::clone_into(path, staged).await
        }
    }
}

/// Tell a watching front end how one repository ended.
///
/// Why: every arm of the acquisition loop ends a unit, including the two that
/// return early, and a display left holding a repository the loop has already
/// moved past is the wedged state #5823 names. One helper means the mapping
/// from acquisition state to display verdict is written once.
/// What: [`CloneState::Reused`] and [`CloneState::Cloned`] are successes;
/// `Empty` is a failure (it is not a checkout anything can read); `Failed` and
/// `Skipped` carry their own reasons.
/// Test: `super::clone_tests::every_acquisition_outcome_is_reported_once`.
fn announce(progress: &Progress, name_with_owner: &str, state: &CloneState) {
    let outcome = match state {
        CloneState::Cloned | CloneState::Reused => UnitOutcome::Succeeded,
        CloneState::Failed(reason) | CloneState::Empty(reason) => {
            UnitOutcome::Failed(reason.clone())
        }
        CloneState::Skipped(reason) => UnitOutcome::Skipped(reason.clone()),
    };
    progress.unit_finished(Operation::CloneRepos, name_with_owner, outcome);
}

#[cfg(test)]
mod clone_tests {
    use super::*;
    use crate::local_repo::local_repo_tests::{run_git, source_repo};
    use trusty_common::gh::GhError;

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
            bytes_complete: true,
        }
    }

    /// A minimal tree `verify_checkout` accepts: HEAD pointing at a ref that exists.
    fn plant_checkout(tree: &Path) {
        std::fs::create_dir_all(tree.join(".git/refs/heads")).expect("mkdir");
        std::fs::write(tree.join(".git/HEAD"), b"ref: refs/heads/main\n").expect("HEAD");
        std::fs::write(tree.join(".git/refs/heads/main"), b"abc123\n").expect("ref");
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

    /// The fail-open regression the review found: a depth-1 fetch of a
    /// commitless repository EXITS ZERO leaving only `.git`. Promoting that is
    /// an audit reporting coverage of a repository nothing ever read.
    #[test]
    fn a_commitless_clone_is_not_a_usable_checkout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dest = tmp.path().join("api");
        let staged = tmp.path().join("staged");
        // Exactly what a commitless clone leaves: .git, HEAD, no such ref.
        std::fs::create_dir_all(staged.join(".git/refs/heads")).expect("mkdir");
        std::fs::write(staged.join(".git/HEAD"), b"ref: refs/heads/main\n").expect("HEAD");

        let (state, bytes, _) = finish_one(&dest, &staged, Ok(()));
        let CloneState::Empty(why) = &state else {
            panic!("a zero exit with no commits must not be Cloned: {state:?}");
        };
        assert!(why.contains("no commits"), "{why}");
        assert!(!state.is_usable());
        assert_eq!(bytes, 0);
        assert!(!dest.exists(), "nothing may be promoted");
        assert!(!staged.exists());
    }

    #[test]
    fn a_tree_with_no_dot_git_is_not_a_checkout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let staged = tmp.path().join("staged");
        std::fs::create_dir_all(&staged).expect("mkdir");
        assert!(verify_checkout(&staged).is_err());
    }

    #[test]
    fn a_checkout_with_a_packed_head_ref_is_usable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let staged = tmp.path().join("staged");
        std::fs::create_dir_all(staged.join(".git")).expect("mkdir");
        std::fs::write(staged.join(".git/HEAD"), b"ref: refs/heads/main\n").expect("HEAD");
        std::fs::write(
            staged.join(".git/packed-refs"),
            b"# pack-refs with: peeled\nabc123 refs/heads/main\n",
        )
        .expect("packed-refs");
        verify_checkout(&staged).expect("a packed ref resolves HEAD");
    }

    #[test]
    fn a_detached_head_is_usable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let staged = tmp.path().join("staged");
        std::fs::create_dir_all(staged.join(".git")).expect("mkdir");
        std::fs::write(staged.join(".git/HEAD"), b"abc123def\n").expect("HEAD");
        verify_checkout(&staged).expect("a detached HEAD names a commit");
    }

    /// The data-loss regression: `.` is a legal name character, so the old
    /// `<dest>.partial` staging path was addressable by the legal repository
    /// `acme/api.partial` — and acquiring `acme/api` deleted it.
    #[tokio::test]
    async fn a_repo_named_like_the_old_staging_path_is_safe() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let victim = destination(&work, "acme/api.partial").expect("a legal name");
        std::fs::create_dir_all(&victim).expect("mkdir");
        std::fs::write(victim.join("f"), b"the recipient's source").expect("write");

        let staged = staging(&work, "acme/api").expect("valid");
        assert_ne!(staged, victim);
        assert!(
            !staged.starts_with(work.path(Area::Repos)),
            "staging must live outside the repository namespace: {}",
            staged.display()
        );

        let report = clone_all(
            &work,
            &["acme/api.partial".to_string(), "acme/api".to_string()],
            &CloneOptions::default(),
            &Progress::none(),
        )
        .await
        .expect("the first repo is usable, so the run continues");
        assert_eq!(report.repos[0].state, CloneState::Reused);
        assert!(
            victim.join("f").is_file(),
            "acquiring acme/api destroyed acme/api.partial's checkout"
        );
    }

    /// #5556: acquiring is selecting. The sweep reads what the clone recorded,
    /// with paths relative to the working-directory root.
    #[tokio::test]
    async fn a_completed_clone_is_recorded_as_the_selection() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        for name in ["acme/api", "acme/web"] {
            let dest = destination(&work, name).expect("valid");
            std::fs::create_dir_all(&dest).expect("mkdir");
            std::fs::write(dest.join("f"), b"source").expect("write");
        }

        clone_all(
            &work,
            &["acme/api".to_string(), "acme/web".to_string()],
            &CloneOptions::default(),
            &Progress::none(),
        )
        .await
        .expect("both checkouts are present, so nothing is fetched");

        let selected = run::load_selection(&work).expect("the sweep's input is there");
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].name, "acme/api");
        assert_eq!(selected[0].path, Path::new("repos/acme/api"));
        assert_eq!(selected[1].name, "acme/web");
    }

    /// An acquisition with nothing usable in it must not clobber the selection
    /// an earlier one recorded — the operator's next `taudit run` still has the
    /// set that did land. (Every repository FAILING is [`summarize`]'s abort;
    /// this is the other empty case, an empty request.)
    #[tokio::test]
    async fn an_empty_request_leaves_an_existing_selection_alone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let dest = destination(&work, "acme/api").expect("valid");
        std::fs::create_dir_all(&dest).expect("mkdir");
        clone_all(
            &work,
            &["acme/api".to_string()],
            &CloneOptions::default(),
            &Progress::none(),
        )
        .await
        .expect("a present checkout needs no network");
        let recorded = std::fs::read_to_string(run::selection_path(&work)).expect("read");

        clone_all(&work, &[], &CloneOptions::default(), &Progress::none())
            .await
            .expect("an empty request is empty, not failed");
        assert_eq!(
            std::fs::read_to_string(run::selection_path(&work)).expect("read"),
            recorded,
            "an empty acquisition rewrote the selection"
        );
    }

    /// A planted `repos/<owner>` symlink is the same escape as a planted
    /// `repos/`, one level down.
    #[tokio::test]
    async fn a_symlinked_owner_directory_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).expect("target");
        std::os::unix::fs::symlink(&outside, work.path(Area::Repos).join("acme")).expect("symlink");

        let err = clone_all(
            &work,
            &["acme/api".to_string()],
            &CloneOptions::default(),
            &Progress::none(),
        )
        .await
        .expect_err("an owner directory that is a symlink must be refused");
        assert!(matches!(err, AuditError::UnsafeArea { .. }), "{err:?}");
        assert!(
            !outside.join("api").exists(),
            "nothing may be written through the symlink"
        );
    }

    /// A DANGLING symlink is the case the old ordering got wrong: `create`
    /// followed it and built the target before the guard ever ran.
    #[tokio::test]
    async fn a_dangling_repos_symlink_is_refused_before_anything_is_created() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = WorkDir::new(tmp.path().join("work"));
        std::fs::create_dir_all(work.root()).expect("root");
        let never = tmp.path().join("never-created");
        std::os::unix::fs::symlink(&never, work.path(Area::Repos)).expect("symlink");

        let err = clone_all(
            &work,
            &["acme/api".to_string()],
            &CloneOptions::default(),
            &Progress::none(),
        )
        .await
        .expect_err("a dangling area symlink must be refused");
        assert!(matches!(err, AuditError::UnsafeArea { .. }), "{err:?}");
        assert!(
            !never.exists(),
            "the guard must run before anything follows the symlink"
        );
    }

    #[test]
    fn an_unreadable_subtree_marks_the_size_incomplete() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let tree = tmp.path().join("tree");
        let blocked = tree.join("blocked");
        std::fs::create_dir_all(&blocked).expect("mkdir");
        std::fs::write(tree.join("f"), b"12345").expect("write");
        std::fs::write(blocked.join("hidden"), b"0123456789").expect("write");
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).expect("chmod");

        let (bytes, complete) = dir_size(&tree);
        // Restore first, so a failed assertion cannot leave an undeletable tree.
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o755))
            .expect("restore");
        assert!(!complete, "an unreadable subtree must not read as a total");
        assert_eq!(bytes, 5, "the readable part is still counted: {bytes}");
    }

    #[test]
    fn a_symlinked_repos_area_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = WorkDir::new(tmp.path().join("work"));
        std::fs::create_dir_all(work.root()).expect("root");
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("target");
        std::os::unix::fs::symlink(&elsewhere, work.path(Area::Repos)).expect("symlink");

        let err =
            ensure_real_dir(work.path(Area::Repos)).expect_err("a symlinked area must be refused");
        let AuditError::UnsafeArea { kind, .. } = &err else {
            panic!("expected UnsafeArea, got {err:?}");
        };
        assert_eq!(*kind, "symlink");
    }

    #[test]
    fn a_real_repos_area_passes_the_guard() {
        let tmp = tempfile::tempdir().expect("tempdir");
        ensure_real_dir(work_in(tmp.path()).path(Area::Repos)).expect("a real directory is fine");
    }

    /// Why (#5916): the argv is where the defect lived. `-- --depth=1` here
    /// made every tga database report one commit by one author over a
    /// zero-length period, and nothing downstream could tell that apart from a
    /// repository that genuinely has one commit.
    /// What: the invocation carries no depth flag and no `--` separator, so
    /// `gh` forwards nothing to `git` that could truncate the history.
    /// Test: this is the test.
    #[test]
    fn a_clone_asks_git_for_the_whole_history() {
        let argv = clone_command("acme/api", Path::new("/w/stage/acme/api")).argv_display();
        assert_eq!(argv, "repo clone acme/api /w/stage/acme/api");
        assert!(!argv.contains("--depth"), "{argv}");
        assert!(!argv.contains(" -- "), "{argv}");
    }

    /// The fail-open regression: a clone that died part-way must not leave a
    /// directory the sweep would later read as a whole repository.
    #[test]
    fn a_failed_clone_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dest = tmp.path().join("api");
        let staged = tmp.path().join("staged");
        plant_checkout(&staged);
        std::fs::write(staged.join("README.md"), b"partial").expect("write");

        let (state, bytes, _) = finish_one(&dest, &staged, Err(gh_failure().to_string()));
        assert!(matches!(state, CloneState::Failed(_)), "{state:?}");
        assert_eq!(bytes, 0);
        assert!(
            !dest.exists(),
            "a failed clone must not appear as a checkout"
        );
        assert!(!staged.exists(), "the staged tree must be removed");
    }

    #[test]
    fn a_successful_clone_is_renamed_into_place() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dest = tmp.path().join("api");
        let staged = tmp.path().join("staged");
        plant_checkout(&staged);
        std::fs::write(staged.join("f"), b"0123456789").expect("write");

        let (state, bytes, complete) = finish_one(&dest, &staged, Ok(()));
        assert_eq!(state, CloneState::Cloned);
        assert!(
            bytes >= 10,
            "the measured tree includes the payload: {bytes}"
        );
        assert!(complete);
        assert!(dest.join(".git").is_dir());
        assert!(!staged.exists());
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

        let report = clone_all(
            &work,
            &["acme/api".to_string()],
            &CloneOptions::default(),
            &Progress::none(),
        )
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
            &Progress::none(),
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
                budget_bytes: Some(4),
            },
            &Progress::none(),
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
    /// #5916: `.git/shallow` is the filesystem marker `git` writes exactly when
    /// a clone was truncated, so asserting its ABSENCE is the depth contract
    /// stated where it can only be true of a real fetch — `clone_command`'s argv
    /// test proves the flag is gone, and this proves the fetch it produces is
    /// whole. Its presence is what made every tga database report one commit.
    /// The measured size is checked against the default budget in the same
    /// breath, because full clones are the ones that could newly exhaust it.
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
            &Progress::none(),
        )
        .await
        .expect("a public repository clones");
        assert_eq!(report.repos[0].state, CloneState::Cloned);
        let checkout = &report.repos[0].path;
        assert!(checkout.join(".git").is_dir());
        assert!(
            !checkout.join(".git/shallow").exists(),
            "the clone was truncated — tga would read one commit by one author"
        );
        assert!(report.total_bytes > 0, "the report must state disk use");
        assert!(
            report.total_bytes < DEFAULT_BUDGET_BYTES,
            "one ordinary full clone must not exhaust the default budget: {} of {DEFAULT_BUDGET_BYTES}",
            report.total_bytes
        );
    }
    /// 🔴 #6001: the whole local path, end to end. A path registers, clones into
    /// `repos/local/<basename>`, verifies, and is recorded as the sweep's
    /// selection — indistinguishable downstream from a remotely-cloned one.
    ///
    /// Against `7eef4bb9b` the spec reaches `split_name`, which refuses a
    /// leading `/`, so `clone_all` returns `InvalidRepoName` and nothing is
    /// acquired at all.
    #[tokio::test]
    async fn a_local_path_is_acquired_under_the_local_owner() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let src = tmp.path().join("apex");
        source_repo(&src);

        let report = clone_all(
            &work,
            &[src.display().to_string()],
            &CloneOptions::default(),
            &Progress::none(),
        )
        .await
        .expect("a local checkout is a usable source");

        assert_eq!(report.repos[0].state, CloneState::Cloned);
        assert_eq!(report.repos[0].name_with_owner, "local/apex");
        assert_eq!(
            report.repos[0].path,
            work.path(Area::Repos).join("local/apex")
        );
        assert!(report.gaps.is_empty(), "{:?}", report.gaps);
        assert!(report.total_bytes > 0, "the report must state disk use");

        // The acquired tree is a whole checkout with the source's history, not
        // a truncated one — #5916's contract, held for this mechanism too.
        let acquired = &report.repos[0].path;
        assert!(!acquired.join(".git/shallow").exists());
        assert_eq!(run_git(acquired, &["rev-list", "--count", "HEAD"]), "2");

        // #5556: acquiring is selecting, on this path as much as the other.
        let selected = run::load_selection(&work).expect("the sweep's input is there");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "local/apex");
        assert_eq!(selected[0].path, Path::new("repos/local/apex"));
    }

    /// 🔴 #6130's registration half: the on-disk identity stays `local/<name>`
    /// and the ISSUE identity is the real slug the source's `origin` names. The
    /// self-audit's own shape — a checkout of `bobmatnyc/trusty-tools` audited
    /// by path.
    ///
    /// Against the pre-fix code `github_slug` does not exist and the sweep
    /// hands `local/trusty-tools` to tga as `github.repo`, which is what 404'd
    /// 3152 of 3152 work-item lookups.
    #[tokio::test]
    async fn a_local_path_with_a_github_origin_records_the_real_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let src = tmp.path().join("trusty-tools");
        source_repo(&src);
        run_git(
            &src,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:bobmatnyc/trusty-tools.git",
            ],
        );

        clone_all(
            &work,
            &[src.display().to_string()],
            &CloneOptions::default(),
            &Progress::none(),
        )
        .await
        .expect("a local checkout is a usable source");

        let selected = run::load_selection(&work).expect("the sweep's input is there");
        assert_eq!(
            selected[0].name, "local/trusty-tools",
            "the on-disk identity"
        );
        assert_eq!(
            selected[0].github_leg(),
            run::GithubLeg::Present("bobmatnyc/trusty-tools"),
            "the issue identity is the remote's, not the directory's"
        );
    }

    /// The other arm: no GitHub remote, so the leg is declared absent with a
    /// sentence the report can print — never a slug the API would refuse.
    #[tokio::test]
    async fn a_local_path_with_no_github_origin_records_why() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let src = tmp.path().join("apex");
        source_repo(&src);

        clone_all(
            &work,
            &[src.display().to_string()],
            &CloneOptions::default(),
            &Progress::none(),
        )
        .await
        .expect("a local checkout is a usable source");

        let selected = run::load_selection(&work).expect("the sweep's input is there");
        let run::GithubLeg::Absent(reason) = selected[0].github_leg() else {
            panic!("a checkout with no remote has no GitHub identity");
        };
        assert!(reason.contains("local/apex"), "{reason}");
        assert!(reason.contains("github.com"), "{reason}");
        assert!(
            reason.contains(&src.display().to_string()),
            "the source the operator named must be in the sentence: {reason}"
        );
    }

    /// A REMOTE entry's issue identity is the slug the request already named,
    /// so the same field serves both shapes and `crate::run` needs no fork.
    #[tokio::test]
    async fn a_remote_entrys_issue_identity_is_its_own_name() {
        let planned = vec![(
            "acme/api".to_string(),
            Source::Remote,
            PathBuf::from("/w/repos/acme/api"),
            PathBuf::from("/w/state/clone-staging/acme/api"),
        )];
        let resolved = resolve_github_identities(&planned).await;
        assert_eq!(
            resolved.get("acme/api"),
            Some(&GithubIdentity::Slug("acme/api".to_string()))
        );
    }

    /// 🔴 Two local paths with one basename derive one destination, and
    /// `clone_all` REUSES a directory that is already there — so without this
    /// guard the second is reported as audited having read the first one's
    /// history. Refused as a whole request, nothing acquired.
    #[tokio::test]
    async fn two_paths_with_one_basename_are_refused_together() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let first = tmp.path().join("a/apex");
        let second = tmp.path().join("b/apex");
        source_repo(&first);
        source_repo(&second);

        let err = clone_all(
            &work,
            &[first.display().to_string(), second.display().to_string()],
            &CloneOptions::default(),
            &Progress::none(),
        )
        .await
        .expect_err("one destination for two repositories must be refused");
        let AuditError::CollidingCheckouts { name, .. } = &err else {
            panic!("expected CollidingCheckouts, got {err:?}");
        };
        assert_eq!(name, "local/apex");
        let rendered = err.to_string();
        assert!(
            rendered.contains(&first.display().to_string()),
            "{rendered}"
        );
        assert!(
            rendered.contains(&second.display().to_string()),
            "{rendered}"
        );
        assert!(
            !work.path(Area::Repos).join("local").exists(),
            "nothing may be acquired when the request is refused"
        );
    }

    /// 🔴 On a case-insensitive, case-preserving filesystem (APFS's default —
    /// the filesystem this feature runs on) `repos/local/Apex` and
    /// `repos/local/apex` are ONE directory, but [`local_repo::derive_name`]
    /// preserves case, so before the collision comparison was case-folded this
    /// case pair was silently misattributed rather than refused: `clone_all`
    /// returned `Ok` with the second repository reported as `CloneState::Reused`
    /// against the first repository's on-disk tree, not `CollidingCheckouts`.
    #[tokio::test]
    async fn two_paths_that_differ_only_by_case_are_refused_together() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let first = tmp.path().join("a/Apex");
        let second = tmp.path().join("b/apex");
        source_repo(&first);
        source_repo(&second);

        let err = clone_all(
            &work,
            &[first.display().to_string(), second.display().to_string()],
            &CloneOptions::default(),
            &Progress::none(),
        )
        .await
        .expect_err("a case-only difference must still be refused as a collision");
        let AuditError::CollidingCheckouts { name, .. } = &err else {
            panic!("expected CollidingCheckouts, got {err:?}");
        };
        assert_eq!(name.to_ascii_lowercase(), "local/apex");
        let rendered = err.to_string();
        assert!(
            rendered.contains(&first.display().to_string()),
            "{rendered}"
        );
        assert!(
            rendered.contains(&second.display().to_string()),
            "{rendered}"
        );
        assert!(
            !work.path(Area::Repos).join("local").exists(),
            "nothing may be acquired when the request is refused"
        );
    }

    /// The same path twice is one checkout, not a collision — a `repos.txt` an
    /// operator listed twice must still run.
    #[tokio::test]
    async fn the_same_path_twice_is_not_a_collision() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let src = tmp.path().join("apex");
        source_repo(&src);
        let spec = src.display().to_string();

        let report = clone_all(
            &work,
            &[spec.clone(), format!("{spec}/")],
            &CloneOptions::default(),
            &Progress::none(),
        )
        .await
        .expect("one repository listed twice is one repository");
        assert_eq!(report.repos[0].state, CloneState::Cloned);
        assert_eq!(report.repos[1].state, CloneState::Reused);
    }

    /// 🔴 A source that was fine at registration and is not fine at sweep time
    /// becomes a NAMED gap. Reporting it as cloned is the fail-open this crate
    /// has shipped three times.
    #[tokio::test]
    async fn a_source_that_went_bad_after_registration_is_a_gap() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let good = tmp.path().join("apex");
        source_repo(&good);
        let gone = tmp.path().join("deleted-since");

        let report = clone_all(
            &work,
            &[good.display().to_string(), gone.display().to_string()],
            &CloneOptions::default(),
            &Progress::none(),
        )
        .await
        .expect("one usable source keeps the sequence going");
        assert_eq!(report.repos[0].state, CloneState::Cloned);
        assert!(
            matches!(report.repos[1].state, CloneState::Failed(_)),
            "{:?}",
            report.repos[1].state
        );
        assert_eq!(report.gaps.len(), 1);
        assert!(
            report.gaps[0].contains("does not exist"),
            "{:?}",
            report.gaps
        );
        assert!(
            !work.path(Area::Repos).join("local/deleted-since").exists(),
            "a failed source must not appear as a checkout"
        );
    }

    /// Why (#5823): every arm of the acquisition loop ends a unit, and two of
    /// them return early. A missed one leaves a display holding a repository
    /// the loop moved past minutes ago — and reports it as still cloning.
    /// What: each acquisition state maps to the verdict a display renders, and
    /// a reused checkout is a success rather than a silent nothing.
    /// Test: this is the test.
    #[test]
    fn every_acquisition_outcome_is_reported_once() {
        let (recorder, progress) = crate::progress::Recorder::new();
        for state in [
            CloneState::Cloned,
            CloneState::Reused,
            CloneState::Failed("remote refused".into()),
            CloneState::Empty("no commits".into()),
            CloneState::Skipped("budget spent".into()),
        ] {
            announce(&progress, "acme/api", &state);
        }

        let verdicts: Vec<UnitOutcome> = recorder
            .updates()
            .into_iter()
            .filter_map(|u| match u {
                crate::progress::ProgressUpdate::UnitFinished { outcome, .. } => Some(outcome),
                _ => None,
            })
            .collect();
        assert_eq!(
            verdicts,
            vec![
                UnitOutcome::Succeeded,
                UnitOutcome::Succeeded,
                UnitOutcome::Failed("remote refused".into()),
                // A commitless checkout is nothing a later stage can read, so
                // it is a failure here even though `gh` exited zero.
                UnitOutcome::Failed("no commits".into()),
                UnitOutcome::Skipped("budget spent".into()),
            ]
        );
    }
}
