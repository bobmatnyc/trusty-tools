//! A broken `install.toml` fails clone and delete paths closed (#11821 P2).
//!
//! Why: df-D1t. With a broken `~/.trusty-tools/install.toml`, the projects root
//! fell back to `~/trusty-mpm-projects`, so a clone, a reuse or a removal could
//! act on a root the operator did not choose. The cold-start, in-project and
//! decommission cases call public API that predates P2, so they compile against
//! P1 and were red there: P1 cloned, reused and decommissioned under the
//! default root.
//! What: each case points `$HOME` at a tempdir holding a broken install file,
//! clears the root env vars, and asserts the call refuses with nothing written
//! under the default root.
//! Test: this file IS the test (mounted in the `env_serial` target).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use trusty_mpm::core::trusty_tools_config::{TrustyToolsConfig, try_workspace_root};
use trusty_mpm::daemon::managed_routes::inproject::{
    base_clone_path, try_base_clone_path, try_inproject_spawn,
};
use trusty_mpm::daemon::managed_routes::inproject_cold_start::ensure_managed_checkout;
use trusty_mpm::session_manager::{ManagedError, ManagedTmuxDriver, SessionManager};

/// The env vars that would decide the root before the install file.
const ROOT_VARS: [&str; 3] = [
    "TRUSTY_MPM_WORKSPACE_ROOT",
    "TRUSTY_MPM_REPOS_ROOT",
    "TRUSTY_MPM_CONFIG_DIR",
];

/// Points `HOME` at a scratch dir, clears [`ROOT_VARS`], restores all on drop.
///
/// `env_serial` runs one test at a time, so no other test observes the rewrite.
struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn install(home: &Path) -> Self {
        let saved = std::iter::once("HOME")
            .chain(ROOT_VARS)
            .map(|k| (k, std::env::var_os(k)))
            .collect();
        // SAFETY: `env_serial` forces one test thread; Drop restores every var.
        unsafe {
            std::env::set_var("HOME", home);
            for k in ROOT_VARS {
                std::env::remove_var(k);
            }
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: as in `install`.
        unsafe {
            for (k, v) in self.saved.drain(..) {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

/// A scratch home with a broken install file.
struct Fixture {
    tmp: tempfile::TempDir,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let home = tmp.path().canonicalize().expect("canonical").join("home");
        let tools = home.join(".trusty-tools");
        std::fs::create_dir_all(&tools).expect("mkdir .trusty-tools");
        // An unknown key: the file parses as TOML but is refused.
        std::fs::write(
            tools.join("install.toml"),
            "projects_root = \"/elsewhere/projects\"\nbogus = 1\n",
        )
        .expect("write install.toml");
        Self { tmp, home }
    }

    /// Where P1 put managed checkouts when the install file was broken.
    fn default_checkout(&self) -> PathBuf {
        self.home
            .join("trusty-mpm-projects")
            .join("acme")
            .join("widget")
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A one-commit repository to clone from.
fn init_source(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).expect("mkdir source");
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "user.name", "Test"]);
    std::fs::write(path.join("README.md"), "hello\n").expect("write");
    git(path, &["add", "."]);
    git(path, &["commit", "-q", "-m", "initial"]);
    path.to_path_buf()
}

/// The resolvers: `try_` refuses, the reader keeps its logged default, and
/// `TRUSTY_MPM_REPOS_ROOT` still wins without reading the file.
#[test]
fn broken_install_file_fails_the_workspace_root_closed() {
    let fx = Fixture::new();
    let _env = EnvGuard::install(&fx.home);

    let root = try_workspace_root(&TrustyToolsConfig::load());
    assert!(
        root.is_err(),
        "a broken install.toml must refuse, got {root:?}"
    );
    let clone = try_base_clone_path("acme", "widget");
    assert!(clone.is_err(), "the clone path must refuse, got {clone:?}");
    assert_eq!(base_clone_path("acme", "widget"), fx.default_checkout());

    let pinned = fx.tmp.path().join("pinned");
    // SAFETY: `env_serial`; `EnvGuard` restores the var.
    unsafe { std::env::set_var("TRUSTY_MPM_REPOS_ROOT", &pinned) };
    assert_eq!(
        try_base_clone_path("acme", "widget").expect("the env var wins"),
        pinned.join("acme").join("widget")
    );
}

/// A cold start refuses before cloning; P1 cloned under the default root.
#[test]
fn cold_start_refuses_on_a_broken_install_file() {
    let fx = Fixture::new();
    let source = init_source(&fx.tmp.path().join("source"));
    let _env = EnvGuard::install(&fx.home);

    let result =
        ensure_managed_checkout("acme", "widget", source.to_str().expect("utf8 path"), None);

    let err = result.expect_err("a broken install.toml must refuse the clone");
    assert!(err.to_string().contains("install.toml"), "got: {err}");
    assert!(
        !fx.default_checkout().exists(),
        "nothing may be cloned under the default root"
    );
}

/// An in-project spawn refuses to reuse a clone found under the default root.
#[test]
fn inproject_spawn_refuses_on_a_broken_install_file() {
    let fx = Fixture::new();
    let source = init_source(&fx.tmp.path().join("source"));
    let stale = fx.default_checkout();
    std::fs::create_dir_all(stale.parent().expect("parent")).expect("mkdir");
    git(
        fx.tmp.path(),
        &[
            "clone",
            "-q",
            source.to_str().expect("utf8"),
            stale.to_str().expect("utf8"),
        ],
    );
    git(
        &stale,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/widget.git",
        ],
    );
    let _env = EnvGuard::install(&fx.home);

    let err = try_inproject_spawn(&stale).expect_err("a broken install.toml must refuse");
    assert!(err.contains("install.toml"), "got: {err}");
}

/// A tmux double with no sessions; decommission must refuse before using it.
struct NoTmux;

impl ManagedTmuxDriver for NoTmux {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }
}

/// Decommission refuses and leaves the record as it was; P1 decommissioned
/// with its removal guard rooted at the default.
#[tokio::test]
async fn decommission_refuses_on_a_broken_install_file() {
    let fx = Fixture::new();
    let store = fx.tmp.path().join("store");
    let workspace = fx.default_checkout().join("ws");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    let _env = EnvGuard::install(&fx.home);
    let mgr = SessionManager::new(&store, Arc::new(NoTmux))
        .await
        .expect("manager");
    let record = mgr
        .create(
            "task".into(),
            Some(workspace.clone()),
            None,
            Some(workspace.clone()),
            Some("https://github.com/acme/widget".into()),
            None,
        )
        .await
        .expect("create");
    let before = format!("{:?}", record.state);

    let result = mgr.decommission(&record.id, None).await;

    assert!(
        result.is_err(),
        "a broken install.toml must refuse decommission"
    );
    let after = mgr.get(&record.id).await.expect("record kept");
    assert_eq!(format!("{:?}", after.state), before, "no state advanced");
    assert!(workspace.exists(), "the workspace must not be removed");
}

/// #11821 df-D1w: a launch under a broken install.toml refuses, naming the
/// file and the bad key, and leaves the owner's last-good record on disk.
///
/// The asserts read `{:?}` and the record file, so this compiles on either
/// side of the signature change: before it, the launch ran with no policy and
/// no notice and the record was retired.
#[test]
fn owner_launch_policy_refuses_and_keeps_last_good_on_a_broken_install_file() {
    use trusty_mpm::core::launch_policy::last_good::last_good_path;
    use trusty_mpm::core::launch_policy::{OwnerInputs, provision_settings, resolve_and_record};
    use trusty_mpm::core::session_profile::SessionProfile;

    let fx = Fixture::new();
    let state = fx.tmp.path().join("state");
    let default_root = fx.home.join("trusty-mpm-projects");
    let cwd = fx.default_checkout();
    std::fs::create_dir_all(&cwd).expect("mkdir checkout");
    let owner_file = default_root.join("acme").join(".trusty-mpm-owner.toml");
    std::fs::write(&owner_file, "[mcp]\ndeny = [\"heygen\"]\n").expect("write owner file");
    // A good policy on record, then the owner file is no longer where the
    // defaulted root would look for it.
    resolve_and_record(&OwnerInputs {
        cwd: &cwd,
        repos_root: &default_root,
        state_root: &state,
    });
    let record = last_good_path(&state, "acme");
    assert!(record.exists(), "setup: a last-good record is on disk");
    std::fs::remove_file(&owner_file).expect("remove owner file");
    let _env = EnvGuard::install(&fx.home);

    let outcome = provision_settings(&state, &cwd, None, SessionProfile::Pm);

    let shown = format!("{outcome:?}");
    assert!(
        shown.starts_with("Err(") && shown.contains("install.toml") && shown.contains("bogus"),
        "the launch must refuse naming install.toml and the bad key, got {shown}"
    );
    assert!(
        record.exists(),
        "an unresolved root must never retire the last-good record"
    );
}

/// #11821 df-D1t: `POST .../managed/prune-worktrees` refuses with a 500 naming
/// install.toml; before P2 it previewed the default root and answered 200.
#[tokio::test]
async fn prune_route_refuses_on_a_broken_install_file() {
    use std::future::IntoFuture as _;
    use trusty_mpm::daemon::{api, state::DaemonState};

    let fx = Fixture::new();
    let _env = EnvGuard::install(&fx.home);
    let state = Arc::new(DaemonState::with_root_isolated_managed(fx.tmp.path().join("fw")).await);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(axum::serve(listener, api::router(state)).into_future());

    let resp = reqwest::Client::new()
        .post(format!(
            "http://{addr}/api/v1/sessions/managed/prune-worktrees"
        ))
        .json(&serde_json::json!({ "dry_run": true }))
        .send()
        .await
        .expect("prune request");
    let status = resp.status();
    let body = resp.text().await.expect("body");

    assert_eq!(
        status.as_u16(),
        500,
        "a broken install.toml must refuse: {body}"
    );
    assert!(body.contains("install.toml"), "got: {body}");
}
