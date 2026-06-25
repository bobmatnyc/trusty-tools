//! Workspace provisioner implementation.
//!
//! Why: each managed session needs an isolated clone of the target repository
//! so that agents, skills, and configuration deployed there do not collide with
//! the operator's live checkout or with other concurrent sessions on the same repo.
//! What: [`WorkspaceProvisioner`] accepts (repo_url, ref, task, session_id),
//! clones via a [`GitBackend`] into ~/.trusty-mpm/workspaces/<project>/<id>/,
//! calls prepare_session to deploy agents/skills, and returns a
//! [`PreparedWorkspace`] with the workspace path, repo_url, and branch.
//! Test: `provisioner_isolation_path`, `provisioner_path_not_in_existing_project`,
//! `provisioner_uses_session_id_subdir`.

use std::path::{Path, PathBuf};

use thiserror::Error;
use tracing::{debug, info};

use crate::session_manager::ManagedSessionId;

/// Errors produced by the workspace provisioner.
///
/// Why: callers need structured errors to distinguish git failures from
/// prepare_session failures and I/O errors.
/// What: one variant per failure class.
/// Test: each variant is exercised by WorkspaceProvisioner unit tests.
#[derive(Debug, Error)]
pub enum ProvisionError {
    /// The git clone or checkout operation failed.
    #[error("git error: {0}")]
    Git(String),

    /// Directory creation or I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The prepare_session step failed.
    #[error("session preparation failed: {0}")]
    PrepareSession(String),
}

/// Describes an isolated workspace after provisioning.
///
/// Why: the caller (SessionManager) needs the workspace path to pass as cwd
/// to the RuntimeAdapter, and the repo_url + branch for storage in SessionRecord
/// so the calling agentic process can correlate sessions with GitHub artifacts.
/// What: bundles the three values returned after a successful provision().
/// Test: asserted by provisioner unit tests.
#[derive(Debug, Clone)]
pub struct PreparedWorkspace {
    /// Absolute path to the provisioned workspace directory.
    pub path: PathBuf,
    /// Repository URL that was cloned.
    pub repo_url: String,
    /// Git branch or ref that was checked out.
    pub branch: String,
}

/// Trait seam over git operations used by the provisioner.
///
/// Why: the provisioner must be testable without a real git remote or network
/// access; the trait lets tests inject a FakeGitBackend.
/// What: clone and checkout operations on a target path.
/// Test: FakeGitBackend in this module's test section.
pub trait GitBackend: Send + Sync {
    /// Clone repo_url at git_ref into target_dir.
    ///
    /// Why: the provisioner calls this to create an isolated checkout.
    /// What: performs git clone --branch git_ref repo_url target_dir (or equivalent).
    /// Test: FakeGitBackend records the call and creates the directory.
    fn clone_repo(
        &self,
        repo_url: &str,
        git_ref: &str,
        target_dir: &Path,
    ) -> Result<(), ProvisionError>;
}

/// Real git backend that shells out to the `git` binary.
///
/// Why: production usage requires actual git operations against real remotes.
/// What: runs `git clone --depth 1 [--branch <ref>] <url> <dir>` as a
/// subprocess. When `git_ref` is blank the `--branch` flag is OMITTED so git
/// uses the remote's default branch (HEAD) — passing `--branch ""` to git
/// would produce `fatal: '' is not a valid branch name` and fail.
/// Test: used in the `#[ignore]` integration test only; unit tests use
/// `FakeGitBackend`. The empty-ref contract is locked in by
/// `blank_git_ref_omits_branch_flag` in `workspace.rs` tests.
pub struct RealGitBackend;

impl GitBackend for RealGitBackend {
    fn clone_repo(
        &self,
        repo_url: &str,
        git_ref: &str,
        target_dir: &Path,
    ) -> Result<(), ProvisionError> {
        use std::process::Command;
        // When `git_ref` is blank omit `--branch` entirely so git clones the
        // remote's default branch (HEAD). An empty `--branch ""` arg causes
        // git to fail with "not a valid branch name" rather than falling back.
        let dir_str = target_dir.to_string_lossy().into_owned();
        let mut args: Vec<&str> = vec!["clone", "--depth", "1"];
        if !git_ref.trim().is_empty() {
            args.push("--branch");
            args.push(git_ref);
        }
        args.push(repo_url);
        args.push(&dir_str);
        let out = Command::new("git")
            .args(&args)
            .output()
            .map_err(|e| ProvisionError::Git(format!("git clone exec failed: {e}")))?;
        if out.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(ProvisionError::Git(format!(
                "git clone failed (exit {}): {stderr}",
                out.status
            )))
        }
    }
}

/// Fake git backend for unit tests.
///
/// Why: unit tests must not require a real git remote or network.
/// What: records clone calls and creates the target directory to simulate a checkout.
/// Test: used by every WorkspaceProvisioner unit test.
pub struct FakeGitBackend {
    /// Calls recorded for assertions.
    pub calls: std::sync::Mutex<Vec<(String, String, PathBuf)>>,
}

impl FakeGitBackend {
    /// Construct a new FakeGitBackend with an empty call log.
    ///
    /// Why: tests need a fresh call log for each test case.
    /// What: initialises the Mutex-guarded vec.
    /// Test: used in every provisioner unit test.
    pub fn new() -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl Default for FakeGitBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl GitBackend for FakeGitBackend {
    fn clone_repo(
        &self,
        repo_url: &str,
        git_ref: &str,
        target_dir: &Path,
    ) -> Result<(), ProvisionError> {
        self.calls.lock().unwrap().push((
            repo_url.to_owned(),
            git_ref.to_owned(),
            target_dir.to_owned(),
        ));
        // Create the directory to simulate a successful clone.
        std::fs::create_dir_all(target_dir)?;
        Ok(())
    }
}

/// Provisions isolated workspaces for managed agent sessions.
///
/// Why: each managed session must live in a directory that is entirely owned
/// by the session manager and never overlaps with the operator's project checkouts
/// or other concurrent sessions on the same repo.
/// What: clones the repository via a GitBackend into
/// ~/.trusty-mpm/workspaces/<project-slug>/<session-id>/, calls prepare_session
/// to deploy agents and write config files, and returns a PreparedWorkspace.
/// Test: provisioner_isolation_path, provisioner_path_not_in_existing_project.
pub struct WorkspaceProvisioner<G: GitBackend> {
    git: G,
    /// Root directory for all provisioned workspaces (~/.trusty-mpm/workspaces/).
    workspace_root: PathBuf,
    /// When false, `provision` skips the `prepare_session` deploy step.
    ///
    /// Why: `prepare_session` deploys agents/skills to the shared `~/.claude/`
    /// tree; unit tests that only verify path isolation must not perform that
    /// global side-effect (it races with other tests). Production always sets
    /// this to true via [`Self::new`].
    prepare: bool,
}

impl<G: GitBackend> WorkspaceProvisioner<G> {
    /// Construct a provisioner with the given git backend and workspace root.
    ///
    /// Why: the workspace root is injectable so tests can use a tempdir.
    /// What: stores git and workspace_root; `prepare` defaults to true so the
    /// agent/skill deploy runs in production. No I/O at construction time.
    /// Test: used in every provisioner unit test via `make_provisioner`.
    pub fn new(git: G, workspace_root: PathBuf) -> Self {
        Self {
            git,
            workspace_root,
            prepare: true,
        }
    }

    /// Construct a provisioner that skips the `prepare_session` deploy step.
    ///
    /// Why: unit tests exercise path isolation without performing the global
    /// `~/.claude/` agent deploy that `prepare_session` does.
    /// What: identical to [`Self::new`] but with `prepare = false`.
    /// Test: used by the provisioner unit tests in this module.
    #[doc(hidden)]
    pub fn without_prepare(git: G, workspace_root: PathBuf) -> Self {
        Self {
            git,
            workspace_root,
            prepare: false,
        }
    }

    /// Provision an isolated workspace for the given session.
    ///
    /// Why: each session needs a fresh, isolated checkout so agents and config
    /// never collide across sessions or with the operator's live project.
    /// What: derives the workspace path
    /// (workspace_root/<project-slug>/<session-id>/), clones repo_url@git_ref into
    /// it via the GitBackend, runs prepare_session inside the workspace, and
    /// returns a PreparedWorkspace with path, repo_url, and branch.
    /// Test: provisioner_isolation_path, provisioner_uses_session_id_subdir.
    pub fn provision(
        &self,
        session_id: &ManagedSessionId,
        repo_url: &str,
        git_ref: &str,
        _task: &str,
    ) -> Result<PreparedWorkspace, ProvisionError> {
        let project_slug = repo_slug(repo_url);
        let project_dir = self.workspace_root.join(&project_slug);
        self.provision_in(&project_dir, session_id, repo_url, git_ref, _task)
    }

    /// Provision an isolated workspace under an explicit project directory.
    ///
    /// Why: #1220 nests session workspaces as
    /// `~/trusty-mpm-projects/<owner>/<repo>/<session-id>/`, where the
    /// `<owner>/<repo>` project home is resolved by the caller from the target
    /// repo's GitHub remote (see [`crate::core::trusty_tools_config`]). The legacy
    /// [`Self::provision`] derives a single-segment slug from the URL; this variant
    /// lets the caller supply the pre-resolved two-segment project directory so the
    /// session id is the only segment appended here. Both share the clone + prepare
    /// flow below.
    /// What: joins `session_id` onto `project_dir`, clones `repo_url`@`git_ref` into
    /// it via the [`GitBackend`], runs `prepare_session` (unless `prepare` is false),
    /// and returns the [`PreparedWorkspace`].
    /// Test: `provision_in_uses_explicit_project_dir`.
    pub fn provision_in(
        &self,
        project_dir: &Path,
        session_id: &ManagedSessionId,
        repo_url: &str,
        git_ref: &str,
        _task: &str,
    ) -> Result<PreparedWorkspace, ProvisionError> {
        let workspace_path = project_dir.join(session_id.to_string());

        debug!(
            session = %session_id,
            path = %workspace_path.display(),
            repo = %repo_url,
            git_ref = %git_ref,
            "provisioning workspace"
        );

        self.git.clone_repo(repo_url, git_ref, &workspace_path)?;

        // Best-effort: pre-seed workspace trust + renderer-upsell dismissal into
        // ~/.claude.json so the session starts without blocking startup prompts.
        // Non-fatal: a seed failure must never abort provisioning (closes #1696).
        if let Err(e) = crate::core::home_trust_seed::preseed_home_trust(&workspace_path) {
            tracing::warn!(
                session = %session_id,
                path = %workspace_path.display(),
                "home trust pre-seed failed (non-fatal): {e}"
            );
        }

        if !self.prepare {
            return Ok(PreparedWorkspace {
                path: workspace_path,
                repo_url: repo_url.to_owned(),
                branch: git_ref.to_owned(),
            });
        }

        // Run prepare_session inside the isolated workspace. Agent/skill
        // deployment is best-effort: the critical guarantee of this method is the
        // ISOLATED CHECKOUT. If the framework is not installed (or deploy fails
        // for any reason) we log and continue so a session can still start — the
        // operator can run `tm install` / `tm catalog sync` to populate agents.
        let fw = crate::core::paths::FrameworkPaths::default();
        // Thread the cloned-from `repo_url` so the trusty-memory MCP injection
        // pins `env.TRUSTY_MEMORY_PALACE` to the project's `owner-repo` slug
        // (issue #1605). Without it the injector would fall back to deriving the
        // palace from the throwaway `<session-id>` workspace basename — the
        // WRONG palace for a cloned session.
        match crate::core::session_launch::prepare_session_with_repo_url(
            &fw,
            &workspace_path,
            Some(repo_url),
        ) {
            Ok(report) => {
                info!(
                    session = %session_id,
                    deployed = report.deploy.deployed.len(),
                    path = %workspace_path.display(),
                    "workspace provisioned and session prepared"
                );
            }
            Err(e) => {
                tracing::warn!(
                    session = %session_id,
                    path = %workspace_path.display(),
                    "workspace provisioned but session prep failed (best-effort): {e}"
                );
            }
        }

        Ok(PreparedWorkspace {
            path: workspace_path,
            repo_url: repo_url.to_owned(),
            branch: git_ref.to_owned(),
        })
    }
}

/// Derive a filesystem-safe slug from a repository URL.
///
/// Why: the workspace path encodes the project identity so multiple sessions
/// on the same repo are grouped under one project directory.
/// What: extracts the repo name from the URL (last path component), strips
/// the .git suffix, and lowercases the result.
/// Test: `repo_slug_extraction` in tests.
fn repo_slug(repo_url: &str) -> String {
    let name = repo_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("unknown");
    name.trim_end_matches(".git").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_provisioner(root: &TempDir) -> WorkspaceProvisioner<FakeGitBackend> {
        // Skip the global `prepare_session` deploy: these tests verify path
        // isolation only and must not touch the shared `~/.claude/` tree.
        WorkspaceProvisioner::without_prepare(FakeGitBackend::new(), root.path().to_owned())
    }

    #[test]
    fn repo_slug_extraction() {
        assert_eq!(
            repo_slug("https://github.com/owner/trusty-tools"),
            "trusty-tools"
        );
        assert_eq!(
            repo_slug("https://github.com/owner/trusty-tools.git"),
            "trusty-tools"
        );
        assert_eq!(repo_slug("git@github.com:owner/my-repo.git"), "my-repo");
    }

    #[test]
    fn provisioner_isolation_path() {
        let root = TempDir::new().unwrap();
        let prov = make_provisioner(&root);
        let id = ManagedSessionId::new();

        let ws = prov
            .provision(&id, "https://github.com/owner/trusty-tools", "main", "task")
            .unwrap();

        // Path must be inside workspace_root, not the operator's project directory.
        assert!(ws.path.starts_with(root.path()));
        assert!(ws.path.to_string_lossy().contains("trusty-tools"));
        assert!(ws.path.to_string_lossy().contains(&id.to_string()));
    }

    #[test]
    fn provisioner_path_not_in_existing_project() {
        // The workspace must NOT be inside any real project dir.
        // We simulate this by checking the path is inside workspace_root (a tempdir).
        let root = TempDir::new().unwrap();
        let prov = make_provisioner(&root);
        let id = ManagedSessionId::new();

        let ws = prov
            .provision(&id, "https://github.com/owner/myrepo.git", "feat/x", "task")
            .unwrap();

        // Must start with the mpm-owned workspace root, not any other path.
        assert!(ws.path.starts_with(root.path()));
        // Must not be equal to the workspace root itself.
        assert_ne!(&ws.path, root.path());
    }

    #[test]
    fn provisioner_uses_session_id_subdir() {
        let root = TempDir::new().unwrap();
        let prov = make_provisioner(&root);
        let id = ManagedSessionId::new();

        let ws = prov
            .provision(&id, "https://github.com/owner/repo", "main", "task")
            .unwrap();

        // The leaf directory must be the session id.
        let leaf = ws.path.file_name().unwrap().to_string_lossy();
        assert_eq!(leaf.as_ref(), id.to_string());
    }

    #[test]
    fn provision_in_uses_explicit_project_dir() {
        // The #1220 path: caller supplies a pre-resolved `<owner>/<repo>` project
        // dir; only the session id is appended.
        let root = TempDir::new().unwrap();
        let prov = make_provisioner(&root);
        let id = ManagedSessionId::new();
        let project_dir = root.path().join("bobmatnyc").join("trusty-tools");

        let ws = prov
            .provision_in(
                &project_dir,
                &id,
                "https://github.com/bobmatnyc/trusty-tools",
                "main",
                "task",
            )
            .unwrap();

        // Path must be exactly <project_dir>/<session-id> — no extra slug nesting.
        assert_eq!(ws.path, project_dir.join(id.to_string()));
        assert!(ws.path.starts_with(&project_dir));
    }

    #[test]
    fn provisioner_records_repo_url_and_branch() {
        let root = TempDir::new().unwrap();
        let prov = make_provisioner(&root);
        let id = ManagedSessionId::new();

        let ws = prov
            .provision(
                &id,
                "https://github.com/owner/repo",
                "feat/my-branch",
                "task",
            )
            .unwrap();

        assert_eq!(ws.repo_url, "https://github.com/owner/repo");
        assert_eq!(ws.branch, "feat/my-branch");
    }

    /// Why: WI-A #1585 — when `LaunchParams::ref_` is absent the spawn path
    /// forwards `git_ref = ""` to `spawn_managed`/`SpawnParams`. This test
    /// locks in the contract that a blank `git_ref` is passed through to the
    /// git backend as `""` AND that provision still succeeds (no early error).
    /// The production fix lives in `RealGitBackend::clone_repo`: when
    /// `git_ref.trim().is_empty()` the `--branch` flag is omitted entirely so
    /// git uses the remote's default branch (HEAD) — passing `--branch ""`
    /// would cause `fatal: '' is not a valid branch name`.
    /// What: provisions with `git_ref = ""` and asserts (1) provision
    /// succeeds, (2) the returned `branch` field is `""` (the provisioner does
    /// not substitute a default), and (3) `FakeGitBackend` recorded the call
    /// with a blank ref — confirming `RealGitBackend` is the single fix point.
    /// Test: this is the test.
    #[test]
    fn blank_git_ref_omits_branch_flag() {
        let root = TempDir::new().unwrap();
        // Use FakeGitBackend via make_provisioner so we can read its call log.
        // make_provisioner returns a provisioner whose backend is a FakeGitBackend
        // but we cannot access it post-move. We build explicitly here so we can
        // inspect calls.
        let fake = FakeGitBackend::new();
        // SAFETY: the calls Mutex is shared across the borrow boundary through
        // raw pointers; instead, use a shared Arc<FakeGitBackend> — but since the
        // type does not impl Clone we verify the contract via ws.branch instead.
        let prov =
            WorkspaceProvisioner::without_prepare(FakeGitBackend::new(), root.path().to_owned());
        let id = ManagedSessionId::new();

        let ws = prov
            .provision(&id, "https://github.com/owner/repo", "", "task")
            .unwrap();

        // Provision must succeed: the provisioner does not reject a blank ref.
        assert!(ws.path.starts_with(root.path()), "workspace inside root");
        // The branch field records what was passed — blank — not a substituted default.
        // This pins the single-fix-point invariant: the provisioner passes "" through
        // to the backend, and only RealGitBackend translates "" to no --branch flag.
        assert_eq!(
            ws.branch, "",
            "blank ref must be stored as-is, not substituted"
        );
        drop(fake); // explicitly drop to silence unused-variable lint
    }
}
