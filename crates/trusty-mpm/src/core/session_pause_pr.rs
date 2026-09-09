//! Publish a session-pause snapshot as its own PR instead of a commit on the
//! main checkout's current branch (#7282).
//!
//! Why: `.trusty-mpm/sessions/**` is tracked in this repo, and the PM committed
//! each pause snapshot on whatever branch the main checkout happened to be on —
//! local `main`. `main` is PR-only, so every one of those commits stranded:
//! local `main` sat ahead 8 / behind 77, and the `pull --ff-only` refresh
//! `tm-workflow.md` prescribes failed on every session. The owner's ruling is
//! that sessions are live state, so a pause must reach `origin/main` the same
//! way every other change does — through a pushed branch and a PR.
//! What: [`publish_pause_snapshot`] builds the commit with git plumbing against
//! a scratch index, so HEAD, the shared index, and the working tree are never
//! touched: only the caller-supplied `.trusty-mpm/sessions/**` paths enter the
//! tree, `git add` is never run, and another session's dirty file cannot be
//! swept in. The scratch index and the PR body live in a private
//! [`tempfile::TempDir`] this module creates per call and drops on every exit
//! path, so two overlapping pauses cannot share either file. The commit lands
//! on a fresh `chore/sessions-<slug>-<ts>` branch off the project's default
//! branch, is pushed, and the PR is opened and armed through the existing
//! `tm pr open` / `tm pr merge --auto` path — this module spells no
//! `gh pr create` of its own.
//! Test: `session_pause_pr_tests.rs`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::core::attribution::ATTRIBUTION_FOOTER;

/// The only path prefix a pause commit may ever contain.
pub const SESSIONS_PREFIX: &str = ".trusty-mpm/sessions/";

/// The default branch assumed when neither the project config nor
/// `origin/HEAD` names one.
///
/// Why: `session_context_pause` serves every managed project, and a `master` or
/// `develop` project must not be told its pause is illegal. This is the LAST
/// resort in [`resolve_default_branch`], never an override (#7282 review).
pub const FALLBACK_BRANCH: &str = "main";

/// Blob mode for a regular non-executable file, as `update-index` spells it.
const BLOB_MODE: &str = "100644";

/// One completed subprocess, as this module needs it.
///
/// Why: `git check-ignore` uses its exit code as an answer rather than a
/// failure, so the driver hands back the triple and each step decides.
/// What: exit success, stdout, stderr.
/// Test: `FakeVcs` in the sibling test file constructs these directly.
#[derive(Debug, Clone)]
pub struct CmdOut {
    /// Whether the process exited zero.
    pub success: bool,
    /// Verbatim stdout.
    pub stdout: String,
    /// Verbatim stderr.
    pub stderr: String,
}

impl CmdOut {
    /// stdout, trimmed.
    fn out(&self) -> &str {
        self.stdout.trim()
    }

    /// The failure text a step reports: stderr if it said anything, else stdout.
    fn failure(&self) -> String {
        let e = self.stderr.trim();
        if e.is_empty() {
            self.stdout.trim().to_string()
        } else {
            e.to_string()
        }
    }
}

/// The seam every subprocess in this module goes through.
///
/// Why: the whole publish sequence is a decision table over command output, so
/// hiding the spawn behind a trait makes branch naming, the path allowlist, the
/// not-on-the-default-branch refusal, and the push/PR failure arms testable
/// with no git
/// repository, no network, and no `gh`.
/// What: `git` (with an optional `GIT_INDEX_FILE` overlay, which is how the
/// scratch index stays out of the shared one) and `tm`.
/// Test: `FakeVcs` in the sibling test file scripts responses by argv prefix.
pub trait PauseVcs {
    /// Run `git <args>` in `repo`, optionally against a scratch index file.
    fn git(
        &self,
        repo: &Path,
        args: &[String],
        index_file: Option<&Path>,
    ) -> anyhow::Result<CmdOut>;

    /// Run `tm <args>` in `repo`.
    fn tm(&self, repo: &Path, args: &[String]) -> anyhow::Result<CmdOut>;
}

/// What a caller must supply to publish one pause snapshot.
///
/// Why: the snapshot writer already knows the session id, the timestamp, and
/// exactly which files it touched; re-deriving any of them here would be a
/// second answer to a question already settled.
/// What: the checkout, the session id, the pause timestamp, the repo-relative
/// paths to publish, and the project's configured default branch when one is
/// declared. The scratch directory is NOT a field: this module makes its own,
/// so no caller can hand two concurrent pauses the same one (#7282 review).
/// Test: `publish_commits_only_the_allowlisted_paths`.
#[derive(Debug)]
pub struct PublishRequest<'a> {
    /// The git checkout holding `.trusty-mpm/sessions/`.
    pub repo: &'a Path,
    /// The session id the snapshot was filed under.
    pub session_id: &'a str,
    /// The pause timestamp, used for the branch suffix and the title.
    pub timestamp: DateTime<Utc>,
    /// Repo-relative paths to commit. Every one must start with
    /// [`SESSIONS_PREFIX`].
    pub paths: Vec<String>,
    /// The project's configured default branch, when the operator declared one.
    /// `None` falls back to `origin/HEAD`, then to [`FALLBACK_BRANCH`].
    pub default_branch: Option<&'a str>,
}

/// A published pause snapshot.
///
/// Why: the PR is the deliverable, so a failure to ARM auto-merge is reported
/// rather than raised — but it is reported, not dropped. A PR that never merges
/// with nothing naming why is the same silent-failure shape #7282 exists to end.
/// What: the branch, the commit, the PR URL, and whether auto-merge armed —
/// with what the refusal said when it did not.
/// Test: `auto_merge_failure_is_reported_without_failing_the_publish`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishOutcome {
    /// The branch the commit was created on.
    pub branch: String,
    /// The commit sha.
    pub commit: String,
    /// The PR URL `tm pr open` printed.
    pub pr_url: String,
    /// Whether `tm pr merge --auto` armed squash auto-merge.
    pub auto_merge_armed: bool,
    /// What `tm pr merge --auto` reported when it did not arm. `None` whenever
    /// [`Self::auto_merge_armed`] is true.
    pub auto_merge_error: Option<String>,
}

/// Why a publish did not produce a PR.
///
/// Why: "this project does not track its sessions" is a legitimate no-op, while
/// "the push failed after the commit was made" leaves a local commit a person
/// must deal with. Collapsing the two into one string would hide the second.
/// What: `NotTracked` is the no-op; `NotOnDefaultBranch` refuses before
/// anything is created and names the branch it expected; `Step` names the
/// failed step and carries the branch and commit when they already exist.
/// Test: `publish_refuses_when_not_on_the_default_branch`,
/// `push_failure_leaves_the_commit_and_names_it`,
/// `publish_skips_a_directory_that_is_not_a_git_repo`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublishError {
    /// The snapshot path is git-ignored here — this project does not track
    /// session state, so there is nothing to publish.
    #[error("`{0}` is git-ignored in this checkout; session snapshots are not tracked here")]
    NotTracked(String),
    /// The project directory is not a git checkout at all.
    #[error("`{0}` is not a git repository; there is nothing to publish a snapshot into")]
    NotAGitRepo(String),
    /// The checkout is on some branch other than the project's default.
    #[error(
        "pause snapshots publish only from `{expected}`; this checkout is on `{actual}`. \
         The snapshot file was written; commit and open its PR by hand."
    )]
    NotOnDefaultBranch {
        /// The project's default branch, as resolved for this checkout.
        expected: String,
        /// The branch the checkout is actually on.
        actual: String,
    },
    /// A step failed. `commit` is `Some` once the commit exists locally.
    #[error(
        "session-snapshot publish failed at `{step}`: {detail}{}",
        commit_note(.branch.as_deref(), .commit.as_deref())
    )]
    Step {
        /// The step that failed, as a short label.
        step: &'static str,
        /// What the step reported.
        detail: String,
        /// The chore branch, once it has been created.
        branch: Option<String>,
        /// The commit sha, once it exists.
        commit: Option<String>,
    },
}

/// Render the "your commit is still here" tail of a [`PublishError::Step`].
fn commit_note(branch: Option<&str>, commit: Option<&str>) -> String {
    match (branch, commit) {
        (Some(b), Some(c)) => {
            format!(" — the commit {c} is on local branch `{b}`; push it and open its PR by hand.")
        }
        _ => " — no commit was created.".to_string(),
    }
}

/// Publish `req`'s snapshot paths as a pushed branch and an auto-merging PR.
///
/// Why: see the module doc — a pause must reach `origin/main`, and it must do
/// so without disturbing a checkout other sessions are working in.
/// What: refuses unless the checkout is on the project's default branch and the
/// paths are tracked regular files, then builds the tree with `hash-object` /
/// `read-tree` / `update-index` / `write-tree` against a scratch
/// `GIT_INDEX_FILE` in a per-call [`tempfile::TempDir`], commits with
/// `commit-tree` parented on `origin/<default>`, points a fresh
/// `chore/sessions-<slug>-<ts>` branch at it, pushes that ref, and hands the PR
/// to `tm pr open` and `tm pr merge --auto`. Nothing here runs `git add`,
/// `git stash`, or `git checkout`, so the working tree, HEAD, and the shared
/// index are untouched.
/// Test: `publish_commits_only_the_allowlisted_paths`,
/// `publish_branch_name_carries_session_and_timestamp`,
/// `publish_refuses_when_not_on_the_default_branch`,
/// `publish_uses_the_configured_default_branch`,
/// `concurrent_publishes_never_share_a_scratch_index`,
/// `push_failure_leaves_the_commit_and_names_it`,
/// `publish_is_a_noop_when_the_tree_is_unchanged`,
/// `publish_rejects_a_symlinked_sessions_directory`,
/// `auto_merge_failure_is_reported_without_failing_the_publish`.
pub fn publish_pause_snapshot<V: PauseVcs>(
    vcs: &V,
    req: &PublishRequest<'_>,
) -> Result<Option<PublishOutcome>, PublishError> {
    let paths = allowlisted(req.repo, &req.paths)?;

    // A project directory that is not a checkout at all publishes nothing — the
    // same no-op as a project that git-ignores its sessions, not a failure.
    let branch_out = vcs
        .git(
            req.repo,
            &owned(&["rev-parse", "--abbrev-ref", "HEAD"]),
            None,
        )
        .map_err(|e| step_err("branch", e.to_string(), None, None))?;
    if !branch_out.success {
        return Err(PublishError::NotAGitRepo(req.repo.display().to_string()));
    }
    let current = branch_out.out().to_string();
    let default = resolve_default_branch(vcs, req);
    if current != default {
        return Err(PublishError::NotOnDefaultBranch {
            expected: default,
            actual: current,
        });
    }

    // Exit 0 from `check-ignore` means the path IS ignored: this project keeps
    // its sessions machine-local, so there is nothing to publish.
    let ignored = vcs
        .git(
            req.repo,
            &owned(&["check-ignore", "-q", "--", &paths[0]]),
            None,
        )
        .map_err(|e| step_err("check-ignore", e.to_string(), None, None))?;
    if ignored.success {
        return Err(PublishError::NotTracked(paths[0].clone()));
    }

    git(vcs, req, &["fetch", "origin", &default], None, "fetch")?;
    let origin_ref = format!("origin/{default}");
    let base = git(vcs, req, &["rev-parse", &origin_ref], None, "rev-parse")?
        .out()
        .to_string();
    let base_tree = git(
        vcs,
        req,
        &["rev-parse", &format!("{base}^{{tree}}")],
        None,
        "rev-parse",
    )?
    .out()
    .to_string();

    // #7282 review: the scratch index and the PR body are per-call. Two pauses
    // overlapping — two sessions, or two projects on one host — sharing one
    // `GIT_INDEX_FILE` across the non-atomic read-tree/update-index/write-tree
    // sequence would let one PR carry the other's tree with every git step
    // exiting 0. `TempDir` also removes both files on every exit path,
    // including the early-return errors below.
    let scratch =
        tempfile::TempDir::new().map_err(|e| step_err("scratch", e.to_string(), None, None))?;
    let index = scratch.path().join("index");
    let idx = Some(index.as_path());

    let mut blobs: Vec<(String, String)> = Vec::with_capacity(paths.len());
    for rel in &paths {
        let abs = req.repo.join(rel);
        let sha = git(
            vcs,
            req,
            &[
                "hash-object",
                "-w",
                "--path",
                rel,
                "--",
                &abs.to_string_lossy(),
            ],
            None,
            "hash-object",
        )?
        .out()
        .to_string();
        blobs.push((rel.clone(), sha));
    }

    git(vcs, req, &["read-tree", &base], idx, "read-tree")?;
    for (rel, sha) in &blobs {
        git(
            vcs,
            req,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("{BLOB_MODE},{sha},{rel}"),
            ],
            idx,
            "update-index",
        )?;
    }
    let tree = git(vcs, req, &["write-tree"], idx, "write-tree")?
        .out()
        .to_string();

    if tree == base_tree {
        // Every snapshot path already matches `origin/main`; an empty PR helps
        // nobody, and a caller that treated this as a failure would report one
        // on every re-run.
        return Ok(None);
    }

    let branch = branch_name(req.session_id, req.timestamp);
    let title = pr_title(req.session_id, req.timestamp);
    let message = format!("{title}\n\n{ATTRIBUTION_FOOTER}\n");
    let commit = git(
        vcs,
        req,
        &["commit-tree", &tree, "-p", &base, "-m", &message],
        None,
        "commit-tree",
    )?
    .out()
    .to_string();

    let branch_ref = format!("refs/heads/{branch}");
    git_at(
        vcs,
        req,
        &["update-ref", &branch_ref, &commit],
        "update-ref",
        &branch,
        &commit,
    )?;
    git_at(
        vcs,
        req,
        &["push", "origin", &format!("{branch_ref}:{branch_ref}")],
        "push",
        &branch,
        &commit,
    )?;

    let body_path = scratch.path().join("pr-body.md");
    std::fs::write(&body_path, pr_body(req.session_id, &paths)).map_err(|e| {
        step_err(
            "pr-body",
            e.to_string(),
            Some(branch.clone()),
            Some(commit.clone()),
        )
    })?;

    let open = vcs
        .tm(
            req.repo,
            &owned(&[
                "pr",
                "open",
                "--title",
                &title,
                "--body-file",
                &body_path.to_string_lossy(),
                "--base",
                &default,
                "--docs-only",
                "--rung",
                "1",
                "--session",
                req.session_id,
            ]),
        )
        .map_err(|e| {
            step_err(
                "pr-open",
                e.to_string(),
                Some(branch.clone()),
                Some(commit.clone()),
            )
        })?;
    if !open.success {
        return Err(step_err(
            "pr-open",
            open.failure(),
            Some(branch),
            Some(commit),
        ));
    }
    let (pr_number, pr_url) = parse_pr(&open.stdout).ok_or_else(|| {
        step_err(
            "pr-open",
            format!(
                "could not read a PR number from `tm pr open` output: {}",
                open.out()
            ),
            Some(branch.clone()),
            Some(commit.clone()),
        )
    })?;

    // Arming is reported, never enforced: the PR exists and is the deliverable,
    // so a refusal here must not read as "the snapshot was not published". What
    // the refusal SAID is kept, though — dropping it left a false `armed` and a
    // PR that never merges with nothing anywhere naming why (#7282 review
    // round 3).
    let auto_merge_error = match vcs.tm(req.repo, &owned(&["pr", "merge", &pr_number, "--auto"])) {
        Ok(o) if o.success => None,
        Ok(o) => Some(o.failure()),
        Err(e) => Some(e.to_string()),
    };
    if let Some(detail) = &auto_merge_error {
        tracing::warn!(
            "session-snapshot PR {pr_url} was opened but auto-merge did not arm: {detail}"
        );
    }

    Ok(Some(PublishOutcome {
        branch,
        commit,
        pr_url,
        auto_merge_armed: auto_merge_error.is_none(),
        auto_merge_error,
    }))
}

/// Reject any path outside [`SESSIONS_PREFIX`], and any empty request.
///
/// Why: this is the whole path allowlist. Because the tree is assembled from
/// exactly these entries and `git add` is never run, a dirty file belonging to
/// another session cannot reach the commit — but only if nothing else can be
/// named here either. A symlink is refused here rather than at `hash-object`,
/// which would follow it and commit whatever it points at under a
/// sessions-tree name (#7282 review).
/// What: requires a non-empty list, and every entry to start with the prefix,
/// to contain no `..` segment, and to pass [`walk_is_unredirected`].
/// Test: `publish_rejects_a_path_outside_the_sessions_tree`,
/// `publish_rejects_a_snapshot_path_that_is_not_a_regular_file`,
/// `publish_rejects_a_symlinked_sessions_directory`.
fn allowlisted(repo: &Path, paths: &[String]) -> Result<Vec<String>, PublishError> {
    if paths.is_empty() {
        return Err(step_err(
            "allowlist",
            "no snapshot paths given".into(),
            None,
            None,
        ));
    }
    for p in paths {
        if !p.starts_with(SESSIONS_PREFIX) || p.split('/').any(|seg| seg == "..") {
            return Err(step_err(
                "allowlist",
                format!("`{p}` is outside `{SESSIONS_PREFIX}` and may not be committed by a pause"),
                None,
                None,
            ));
        }
        walk_is_unredirected(repo, p).map_err(|d| step_err("allowlist", d, None, None))?;
    }
    Ok(paths.to_vec())
}

/// Refuse `p` when any segment under `repo` is a symlink, or its leaf is not a
/// regular file.
///
/// Why: `symlink_metadata` on the whole path answers about the LEAF only —
/// every ancestor segment is followed first. A `.trusty-mpm/sessions` that is
/// itself a symlink to a directory outside the checkout therefore left the leaf
/// reporting as an ordinary regular file, and `hash-object` committed the
/// redirected content under a sessions-tree name (#7282 review round 3).
/// What: walks `p` segment by segment from `repo`, `symlink_metadata`s each,
/// and refuses on the first symlink. `repo` itself is not walked — the caller
/// chose it. A segment that does not exist ends the walk, because nothing below
/// it can exist either and `hash-object`'s own error already names it.
/// Test: `publish_rejects_a_symlinked_sessions_directory`,
/// `publish_rejects_a_snapshot_path_that_is_not_a_regular_file`.
fn walk_is_unredirected(repo: &Path, p: &str) -> Result<(), String> {
    let segments: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    let leaf = segments.len().saturating_sub(1);
    let mut at = repo.to_path_buf();
    for (i, segment) in segments.iter().enumerate() {
        at.push(segment);
        let Ok(meta) = std::fs::symlink_metadata(&at) else {
            return Ok(());
        };
        if i == leaf {
            // `symlink_metadata` reports a symlink as neither file nor dir, so
            // this one arm refuses a leaf symlink and a leaf directory alike.
            if !meta.is_file() {
                return Err(format!(
                    "`{p}` is not a regular file and may not be committed by a pause"
                ));
            }
        } else if meta.is_symlink() {
            return Err(format!(
                "`{p}` passes through the symlink `{segment}` and may not be committed by a pause"
            ));
        }
    }
    Ok(())
}

/// The branch this project publishes pauses from.
///
/// Why: `session_context_pause` serves every managed project, so a literal
/// `main` made a pause on a `master` or `develop` project always error
/// (#7282 review). The operator's declared answer wins; `origin/HEAD` is what
/// the checkout itself says; [`FALLBACK_BRANCH`] is the last resort, consulted
/// only when neither answered.
/// What: configured branch, else `git rev-parse --abbrev-ref origin/HEAD` with
/// its `origin/` prefix stripped, else `main`.
/// Test: `publish_uses_the_configured_default_branch`,
/// `publish_falls_back_to_origin_head_for_the_default_branch`,
/// `publish_falls_back_to_main_when_nothing_names_a_branch`.
fn resolve_default_branch<V: PauseVcs>(vcs: &V, req: &PublishRequest<'_>) -> String {
    if let Some(b) = req.default_branch.map(str::trim).filter(|b| !b.is_empty()) {
        return b.to_string();
    }
    if let Ok(out) = vcs.git(
        req.repo,
        &owned(&["rev-parse", "--abbrev-ref", "origin/HEAD"]),
        None,
    ) && out.success
        && let Some(b) = out.out().strip_prefix("origin/")
        && !b.is_empty()
    {
        return b.to_string();
    }
    FALLBACK_BRANCH.to_string()
}

/// `chore/sessions-<slug>-<YYYYMMDD-HHMMSS>`.
///
/// Why/What/Test: one branch per pause, named so a human reading `gh pr list`
/// can tell which session and which pause it came from.
/// `publish_branch_name_carries_session_and_timestamp`.
fn branch_name(session_id: &str, ts: DateTime<Utc>) -> String {
    format!(
        "chore/sessions-{}-{}",
        slug(session_id),
        ts.format("%Y%m%d-%H%M%S")
    )
}

/// The PR and commit-subject line.
fn pr_title(session_id: &str, ts: DateTime<Utc>) -> String {
    format!(
        "chore(sessions): pause snapshot for {session_id} {}",
        ts.format("%Y-%m-%d %H:%MZ")
    )
}

/// Lowercase, hyphenate, collapse, and bound a session id for a branch name.
fn slug(session_id: &str) -> String {
    let mut out = String::new();
    for ch in session_id.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    let bounded: String = trimmed.chars().take(40).collect();
    let bounded = bounded.trim_end_matches('-').to_string();
    if bounded.is_empty() {
        "session".to_string()
    } else {
        bounded
    }
}

/// The seven-field PR body `tm pr open` validates.
fn pr_body(session_id: &str, paths: &[String]) -> String {
    let files = paths
        .iter()
        .map(|p| format!("- `{p}`"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "## Outcome\n\
         Session `{session_id}` paused; its snapshot reaches `origin/main` as a PR \
         rather than a stranded commit on the main checkout.\n\n\
         ## Changes\n{files}\n\n\
         ## Risk\n\
         None. Session state only — no crate source, no CI configuration.\n\n\
         ## Tests\n\
         Rung 1 (docs-only): no Cargo gate applies.\n\n\
         ## Baseline\n\
         No pre-existing failures relevant to this diff.\n\n\
         ## Docs\n\
         No changelog fragment owed — `.trusty-mpm/sessions/**` is not crate source.\n\n\
         ## Review\n\
         Machine-generated pause snapshot; no review findings.\n\n\
         {ATTRIBUTION_FOOTER}\n"
    )
}

/// Read `#<n>` and the URL out of `tm pr open`'s success line.
fn parse_pr(stdout: &str) -> Option<(String, String)> {
    let line = stdout
        .lines()
        .find(|l| l.contains("http") && l.contains('#'))?;
    let url = line.split_whitespace().find(|w| w.starts_with("http"))?;
    let number = url.rsplit('/').next()?;
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((number.to_string(), url.to_string()))
}

/// Build a [`PublishError::Step`].
fn step_err(
    step: &'static str,
    detail: String,
    branch: Option<String>,
    commit: Option<String>,
) -> PublishError {
    PublishError::Step {
        step,
        detail,
        branch,
        commit,
    }
}

/// Run one git step, failing the publish with no commit attributed.
fn git<V: PauseVcs>(
    vcs: &V,
    req: &PublishRequest<'_>,
    args: &[&str],
    index_file: Option<&Path>,
    step: &'static str,
) -> Result<CmdOut, PublishError> {
    let out = vcs
        .git(req.repo, &owned(args), index_file)
        .map_err(|e| step_err(step, e.to_string(), None, None))?;
    if out.success {
        Ok(out)
    } else {
        Err(step_err(step, out.failure(), None, None))
    }
}

/// Run one git step once the commit exists, so a failure names where it is.
fn git_at<V: PauseVcs>(
    vcs: &V,
    req: &PublishRequest<'_>,
    args: &[&str],
    step: &'static str,
    branch: &str,
    commit: &str,
) -> Result<CmdOut, PublishError> {
    let at = || (Some(branch.to_string()), Some(commit.to_string()));
    let out = vcs.git(req.repo, &owned(args), None).map_err(|e| {
        let (b, c) = at();
        step_err(step, e.to_string(), b, c)
    })?;
    if out.success {
        Ok(out)
    } else {
        let (b, c) = at();
        Err(step_err(step, out.failure(), b, c))
    }
}

/// `&[&str]` to `Vec<String>`, which is what [`PauseVcs`] takes.
fn owned(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_string()).collect()
}

/// Production [`PauseVcs`].
///
/// Why: `trusty_common::git::command_in` is this workspace's single git spawn
/// entry point (#7171) — a bare `Command::new("git")` here would reintroduce
/// the auto-maintenance stampede it exists to prevent. `tm` is resolved through
/// `bin_resolve`, the same way every other in-process spawn of a workspace
/// binary resolves one.
/// What: git runs with an optional `GIT_INDEX_FILE`; `tm` runs from the
/// resolved binary with the repo as its working directory.
/// Test: exercised live; every decision it feeds is covered against `FakeVcs`.
pub struct RealPauseVcs;

impl PauseVcs for RealPauseVcs {
    fn git(
        &self,
        repo: &Path,
        args: &[String],
        index_file: Option<&Path>,
    ) -> anyhow::Result<CmdOut> {
        let mut cmd = trusty_common::git::command_in(repo);
        cmd.args(args);
        if let Some(idx) = index_file {
            cmd.env("GIT_INDEX_FILE", idx);
        }
        let out = cmd.output()?;
        Ok(CmdOut {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    fn tm(&self, repo: &Path, args: &[String]) -> anyhow::Result<CmdOut> {
        let bin: PathBuf = trusty_common::bin_resolve::resolve_binary("tm")
            .ok_or_else(|| anyhow::anyhow!("`tm` is not on PATH; cannot open the snapshot PR"))?;
        let out = std::process::Command::new(bin)
            .args(args)
            .current_dir(repo)
            .output()?;
        Ok(CmdOut {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
#[path = "session_pause_pr_tests.rs"]
mod tests;
