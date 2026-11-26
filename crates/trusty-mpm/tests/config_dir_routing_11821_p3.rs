//! trusty-mpm's config trees follow the configured config dir (#11821 P3).
//!
//! Why: before P3, `FrameworkPaths::default()` and every `home.join(".trusty-mpm")`
//! site derived `~/.trusty-mpm` and `~/.trusty-tools` from `$HOME` alone, so a
//! `config_dir` in `install.toml` or `TRUSTY_MPM_CONFIG_DIR` moved only the
//! paths `trusty_common::crate_config` builds.
//! What: each case points `$HOME` at a scratch dir, clears the path env vars,
//! writes an `install.toml`, and asserts where the layout lands. A broken file
//! fails the `try_`/`Option` forms closed and leaves the reader on `$HOME`.
//! Test: this file IS the test (mounted in the `env_serial` target).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use trusty_mpm::core::config_base::{
    config_base, config_base_dir, config_base_under, framework_root_dir, framework_root_under,
    tools_root_dir, tools_root_under, try_config_base,
};
use trusty_mpm::core::paths::FrameworkPaths;

/// The env vars that would decide a path before the install file.
const PATH_VARS: [&str; 4] = [
    "TRUSTY_MPM_CONFIG_DIR",
    "TRUSTY_MPM_WORKSPACE_ROOT",
    "TRUSTY_MPM_REPOS_ROOT",
    "CLAUDE_CONFIG_DIR",
];

/// The env vars that pick the operator's `gh` config dir (#11821 P3-f1).
const GH_VARS: [&str; 2] = ["GH_CONFIG_DIR", "XDG_CONFIG_HOME"];

/// Points `HOME` at a scratch dir, clears [`PATH_VARS`] and [`GH_VARS`],
/// restores all on drop.
///
/// `env_serial` runs one test at a time, so no other test observes the rewrite.
struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn install(home: &Path) -> Self {
        let saved = std::iter::once("HOME")
            .chain(PATH_VARS)
            .chain(GH_VARS)
            .map(|k| (k, std::env::var_os(k)))
            .collect();
        // SAFETY: `env_serial` forces one test thread; Drop restores every var.
        unsafe {
            std::env::set_var("HOME", home);
            for k in PATH_VARS.into_iter().chain(GH_VARS) {
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

/// A scratch `home` and a sibling `cfg`, both canonical.
struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    cfg: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical");
        let home = root.join("home");
        std::fs::create_dir_all(home.join(trusty_common::install_paths::TOOLS_DIR_NAME))
            .expect("mkdir .trusty-tools");
        Self {
            _tmp: tmp,
            home,
            cfg: root.join("cfg"),
        }
    }

    fn write_install_file(&self, body: &str) {
        std::fs::write(
            trusty_common::install_paths::install_file_path(&self.home),
            body,
        )
        .expect("install.toml");
    }
}

/// A relocated config dir moves `.trusty-mpm` and `.trusty-tools`, never
/// `.claude`; on `main` every one of these answered under `$HOME`.
#[test]
fn framework_and_tools_dirs_nest_under_the_config_base() {
    let fx = Fixture::new();
    fx.write_install_file(&format!(
        "config_dir = {:?}\n",
        fx.cfg.display().to_string()
    ));
    let _env = EnvGuard::install(&fx.home);
    let fw_root = fx.cfg.join(".trusty-mpm");
    let tools = fx.cfg.join(".trusty-tools");

    assert_eq!(try_config_base().expect("resolves"), fx.cfg);
    assert_eq!(config_base_dir(), Some(fx.cfg.clone()));
    assert_eq!(framework_root_dir(), Some(fw_root.clone()));
    assert_eq!(tools_root_dir(), Some(tools.clone()));
    assert_eq!(config_base_under(&fx.home), fx.cfg);
    assert_eq!(framework_root_under(&fx.home), fw_root);
    assert_eq!(tools_root_under(&fx.home), tools);

    let fw = FrameworkPaths::default();
    assert_eq!(fw.root, fw_root);
    assert_eq!(
        fw.agent_deploy,
        tools.join("trusty-mpm/claude-config/agents")
    );
    assert_eq!(fw.claude_agents, fx.home.join(".claude/agents"));
    assert_eq!(fw.claude_home_dir(), fx.home);
    assert_eq!(FrameworkPaths::try_default().expect("resolves"), fw);
    assert_eq!(FrameworkPaths::under_home(&fx.home), fw);
    assert_eq!(
        FrameworkPaths::from_root(&fw_root).claude_home_dir(),
        fx.home
    );
    assert_eq!(
        FrameworkPaths::for_managed_workspace(fx.home.join("ws")).root,
        fw_root
    );
    assert_eq!(
        trusty_mpm::core::discovery::lock_file_path(),
        fw_root.join("daemon.lock")
    );
    assert_eq!(
        trusty_mpm::core::trusty_tools_config::managed_claude_config_dir(),
        Some(tools.join("trusty-mpm/claude-config"))
    );

    // The env var outranks the file, as everywhere on the chain.
    let env_cfg = fx.cfg.with_file_name("env-cfg");
    // SAFETY: `env_serial`; `EnvGuard` restores the var.
    unsafe { std::env::set_var("TRUSTY_MPM_CONFIG_DIR", &env_cfg) };
    assert_eq!(FrameworkPaths::default().root, env_cfg.join(".trusty-mpm"));
}

/// A broken install.toml: the `try_` and `Option` forms refuse; the reader
/// forms keep `$HOME`, logged, as `resolve_workspace_root` does.
#[test]
fn config_base_follows_the_install_paths_chain() {
    let fx = Fixture::new();
    fx.write_install_file("config_dir = \"/elsewhere\"\nbogus = 1\n");
    let _env = EnvGuard::install(&fx.home);

    let err = try_config_base().expect_err("a broken install.toml must refuse");
    let shown = err.to_string();
    assert!(
        shown.contains("install.toml") && shown.contains("bogus"),
        "the error must name install.toml and the bad key, got {shown}"
    );
    assert_eq!(config_base_dir(), None);
    assert_eq!(framework_root_dir(), None);
    assert_eq!(tools_root_dir(), None);
    assert!(FrameworkPaths::try_default().is_err());
    assert_eq!(trusty_mpm::core::sm::prompt::sm_override_dir(), None);

    assert_eq!(config_base(), fx.home);
    assert_eq!(config_base_under(&fx.home), fx.home);
    assert_eq!(FrameworkPaths::default(), FrameworkPaths::under(&fx.home));
}

/// A scratch home whose install.toml is broken, `$HOME` pointed at it.
fn broken_install_home() -> (Fixture, EnvGuard) {
    let fx = Fixture::new();
    fx.write_install_file("config_dir = \"/elsewhere\"\nbogus = 1\n");
    let env = EnvGuard::install(&fx.home);
    (fx, env)
}

/// The refusal names install.toml, and nothing landed under `$HOME`.
fn assert_refused_closed(fx: &Fixture, err: String) {
    assert!(
        err.contains("install.toml"),
        "must name install.toml: {err}"
    );
    for tree in [".trusty-mpm", ".trusty-tools/trusty-mpm"] {
        assert!(
            !fx.home.join(tree).exists(),
            "no writer may create $HOME/{tree}"
        );
    }
}

/// #11821 P3 (H3): the pause writer refuses a broken install.toml; before the
/// fix it wrote `$HOME/.trusty-mpm/sessions`, stranded once the file is fixed.
#[test]
fn session_store_writers_fail_closed_on_a_broken_install_file() {
    use trusty_mpm::core::session::{ControlModel, Session, SessionId};
    let (fx, _env) = broken_install_home();
    let session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
    let err = trusty_mpm::core::session_store::save_pause(&session).expect_err("save refuses");
    assert_refused_closed(&fx, err.to_string());
    let err = trusty_mpm::core::session_store::clear_pause(&session.id).expect_err("clear refuses");
    assert_refused_closed(&fx, err.to_string());
}

/// #11821 P3 (H3): the worktree ledger's writer constructors refuse.
#[test]
fn worktree_ledger_writers_fail_closed_on_a_broken_install_file() {
    use trusty_mpm::core::worktree_ledger::WorktreeLedger;
    let (fx, _env) = broken_install_home();
    let err = WorktreeLedger::host().expect_err("host refuses");
    assert_refused_closed(&fx, err.to_string());
    let err = WorktreeLedger::try_under_home(&fx.home).expect_err("try_under_home refuses");
    assert_refused_closed(&fx, err.to_string());
}

/// #11821 P3 (H3): `tm auth set-token`/`clear-token` refuse, naming the file.
#[test]
fn oauth_token_writers_fail_closed_on_a_broken_install_file() {
    let (fx, _env) = broken_install_home();
    let err = trusty_mpm::core::oauth_token::set_token("tok").expect_err("set refuses");
    assert_refused_closed(&fx, err.to_string());
    let err = trusty_mpm::core::oauth_token::clear_token().expect_err("clear refuses");
    assert_refused_closed(&fx, err.to_string());
}

/// #11821 P3 (H3): the build-lease store never resolves to the home's store.
#[test]
fn build_lease_store_fails_closed_on_a_broken_install_file() {
    let (fx, _env) = broken_install_home();
    let err = trusty_mpm::core::build_lease::store::try_canonical_path(&fx.home)
        .expect_err("store path refuses");
    assert_refused_closed(&fx, err.to_string());
}

/// #11821 P3-f1: `--account-token-stdin` refuses before it touches the token.
/// Before the fix the state root fell back to `$HOME/.trusty-mpm`. A blank
/// token keeps the pre-fix run off the network: there the refusal named the
/// empty token, not install.toml.
#[test]
fn gh_session_token_writer_fails_closed_on_a_broken_install_file() {
    let (fx, _env) = broken_install_home();
    let err = trusty_mpm::core::gh_session_account::prepare_session_account(
        "octo",
        "https://github.com/acme/x",
        Some(" "),
    )
    .expect_err("the token writer refuses");
    assert_refused_closed(&fx, err);
}

/// #11821 P3-f1: the console's auto-resume toggle refuses; before the fix it
/// wrote `$HOME/.trusty-mpm/auto_resume`.
#[test]
fn auto_resume_writer_fails_closed_on_a_broken_install_file() {
    let (fx, _env) = broken_install_home();
    let err = trusty_mpm::core::auto_resume::write_desired(true).expect_err("write refuses");
    assert_refused_closed(&fx, err.to_string());
}

/// #11821 P3-f1: the per-login gh account dir refuses; before the fix it was
/// built under `$HOME/.trusty-mpm/gh-accounts`.
#[test]
fn account_config_dir_writer_fails_closed_on_a_broken_install_file() {
    let (fx, _env) = broken_install_home();
    let operator_gh = fx.cfg.with_file_name("operator-gh");
    std::fs::create_dir_all(&operator_gh).expect("mkdir operator gh dir");
    std::fs::write(
        operator_gh.join("hosts.yml"),
        "github.com:\n    users:\n        octo:\n    user: octo\n",
    )
    .expect("hosts.yml");
    // SAFETY: `env_serial`; `EnvGuard` restores the var.
    unsafe { std::env::set_var("GH_CONFIG_DIR", &operator_gh) };
    let err = trusty_mpm::daemon::managed_routes::inproject::account_config_dir_for("octo")
        .expect_err("the account dir refuses");
    assert_refused_closed(&fx, err);
}

/// #11821 P3-f1: the session archive root refuses; before the fix it was
/// `$HOME/.trusty-tools/archive`.
#[test]
fn archive_root_fails_closed_on_a_broken_install_file() {
    let (fx, _env) = broken_install_home();
    let err = trusty_mpm::session_manager::archive_move::default_archive_root()
        .expect_err("the archive root refuses");
    assert_refused_closed(&fx, err.to_string());
}

/// #11821 P3-f1 review: a `github.token_env` launch refuses before it proves
/// or stores the token; before the fix the state root fell back to
/// `$HOME/.trusty-mpm`. An invalid `host` keeps the pre-fix run off the
/// network: there the refusal named the host, not install.toml.
/// #9123: a credential test, so `#[serial]` inside `CredentialSandbox`; the
/// fixture's `EnvGuard` drops first and hands `$HOME` back to the sandbox.
#[test]
#[serial_test::serial]
fn token_env_shim_fails_closed_on_a_broken_install_file() {
    const SOURCE: &str = "TM_TEST_11821_P3F1_TOKEN_ENV";
    let mut sandbox = trusty_common::credentials::test_sandbox::CredentialSandbox::enter();
    let (fx, _env) = broken_install_home();
    let cfg = trusty_mpm::core::trusty_tools_config::GithubConfig {
        token_env: Some(SOURCE.to_string()),
        host: Some("not a host!".to_string()),
        ..Default::default()
    };
    sandbox.set(SOURCE, "tok-11821");
    let env = trusty_mpm::core::gh_identity::resolve_gh_env(Some(&cfg));
    sandbox.remove(SOURCE);
    let env = env.expect("a token_env binding resolves");
    let err = trusty_mpm::core::gh_token_env_shim::shim_token_env(&env)
        .expect_err("the token_env shim refuses");
    assert!(!err.contains("tok-11821"), "the token must not leak: {err}");
    assert_refused_closed(&fx, err);
}

/// An origin whose SSH alias no `~/.ssh/config` renames: the launch refuses
/// at the host, before gh or `GET /user`, so a pre-fix run stays offline.
const UNRESOLVED_ORIGIN: &str = "git@nohost-11821:acme/x.git";

/// No gh shim and no per-login account dir landed under the `$HOME` state
/// root, which only the lenient pin reader may use on a broken install.toml.
fn assert_no_gh_writes(fx: &Fixture, err: &str) {
    assert!(
        err.contains("install.toml"),
        "must name install.toml: {err}"
    );
    for tree in ["gh-shim", "gh-accounts"] {
        let path = FrameworkPaths::under(&fx.home).root.join(tree);
        assert!(!path.exists(), "no writer may create {}", path.display());
    }
}

/// #11821 P3-f2: an in-place relaunch of a pinned checkout refuses before it
/// writes the gh shim; before the fix the shim root fell back to
/// `$HOME/.trusty-mpm` and the refusal named the host, not install.toml. The
/// pin itself is read from the `$HOME` registry: readers stay lenient.
/// #9123: a credential test, so `#[serial]` inside `CredentialSandbox`.
#[test]
#[serial_test::serial]
fn pinned_relaunch_env_fails_closed_on_a_broken_install_file() {
    let _sandbox = trusty_common::credentials::test_sandbox::CredentialSandbox::enter();
    let (fx, _env) = broken_install_home();
    // The lenient reader's `$HOME` fallback, where the pin is read from.
    let registry = trusty_mpm::project::registry_data_dir();
    std::fs::create_dir_all(&registry).expect("mkdir registry");
    let row = serde_json::json!({
        "name": "x", "repo_url": UNRESOLVED_ORIGIN, "default_branch": "main",
        "gh_account": "octo",
    });
    std::fs::write(
        registry.join("projects.json"),
        serde_json::to_vec(&serde_json::json!({ "projects": { "x": row } })).expect("json"),
    )
    .expect("projects.json");
    let checkout = fx.cfg.with_file_name("checkout");
    std::fs::create_dir_all(&checkout).expect("mkdir checkout");
    for args in [
        &["init", "-q"][..],
        &["remote", "add", "origin", UNRESOLVED_ORIGIN][..],
    ] {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&checkout)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }
    let err = trusty_mpm::core::gh_session_shim::pinned_relaunch_env(&checkout)
        .expect_err("the relaunch refuses");
    assert_no_gh_writes(&fx, &err);
}

/// #11821 P3-f2: the daemon's session spawn env refuses the gh shim write and
/// fails the session's gh closed with the nobody token; before the fix the
/// shim root fell back to `$HOME/.trusty-mpm` and the warning named the host.
/// #9123: a credential test, so `#[serial]` inside `CredentialSandbox`.
#[test]
#[serial_test::serial]
fn session_spawn_env_cli_fails_closed_on_a_broken_install_file() {
    let _sandbox = trusty_common::credentials::test_sandbox::CredentialSandbox::enter();
    let (fx, _env) = broken_install_home();
    let pinned = trusty_mpm::core::gh_account::PinnedGhIdentity {
        account: Some("octo".to_string()),
        ..Default::default()
    };
    let env = trusty_mpm::core::gh_session_shim::session_spawn_env_cli(&pinned, UNRESOLVED_ORIGIN)
        .expect("an account pin yields an env")
        .expect("the spawn contract never blocks");
    let warning = env.warning.expect("the refusal rides the env");
    assert_no_gh_writes(&fx, &warning);
    assert!(
        env.vars
            .iter()
            .any(|(k, v)| k == "GH_TOKEN" && v == "tm-refused-unverified-gh-account-pin"),
        "the session's gh must authenticate as nobody: {:?}",
        env.vars.iter().map(|(k, _)| k).collect::<Vec<_>>()
    );
}

/// #11821 P3-f2 PR 2: the `.mcp.json` provenance writer records nothing;
/// before the fix it wrote `$HOME/.trusty-mpm/mcp-json-provenance.json`.
#[test]
fn mcp_provenance_writers_fail_closed_on_a_broken_install_file() {
    let (fx, _env) = broken_install_home();
    let target = fx.home.join("above/.mcp.json");
    trusty_mpm::core::mcp_provenance::record_write_best_effort(&target, "{}");
    assert_refused_closed(&fx, "install.toml".to_string());
}

/// #11821 P3-f2 PR 2: the `tm pr` cleanup registry refuses; before the fix
/// `production()` resolved `$HOME/.trusty-mpm/pr-cleanup.json`.
#[test]
fn pr_cleanup_registry_fails_closed_on_a_broken_install_file() {
    let (fx, _env) = broken_install_home();
    let err = trusty_mpm::core::pr_cleanup::CleanupRegistry::production()
        .expect_err("the registry refuses");
    assert_refused_closed(&fx, err.to_string());
}

/// #11821 P3-f2 PR 2: the project-registry dir a writer uses refuses.
#[test]
fn registry_writers_fail_closed_on_a_broken_install_file() {
    let (fx, _env) = broken_install_home();
    let err = trusty_mpm::project::try_registry_data_dir().expect_err("the registry dir refuses");
    assert_refused_closed(&fx, err.to_string());
}

/// #11821 P3-f2 PR 2: the savings ledger path a writer uses refuses.
#[test]
fn default_savings_log_fails_closed_on_a_broken_install_file() {
    let (fx, _env) = broken_install_home();
    let err = trusty_mpm::core::savings::default_savings_log().expect_err("the ledger refuses");
    assert_refused_closed(&fx, err.to_string());
}

/// #11821 P3-f2 PR 2: the SessionStart sweep leaves a staged row alone;
/// before the fix it swept `$HOME/.trusty-mpm/usage/pending-savings` and
/// deleted this stranded row there.
#[test]
fn staged_savings_sweep_fails_closed_on_a_broken_install_file() {
    let (fx, _env) = broken_install_home();
    // The lenient `$HOME` fallback root, named through the constant.
    let fallback = fx.home.join(trusty_mpm::core::paths::FRAMEWORK_DIR_NAME);
    let pending = fallback.join("usage/pending-savings");
    std::fs::create_dir_all(&pending).expect("mkdir pending");
    let staged = pending.join("00000000000000aa.json");
    std::fs::write(&staged, "{}").expect("stage");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(13 * 60 * 60);
    std::fs::File::options()
        .write(true)
        .open(&staged)
        .and_then(|f| f.set_modified(old))
        .expect("age the staged row");
    assert!(!trusty_mpm::core::savings_sidecar::emit_staged_row_for_session("sess-11821"));
    assert!(staged.exists(), "the $HOME root must not be swept");
    assert!(
        !fallback.join("usage/savings.jsonl").exists(),
        "no row appended under $HOME"
    );
}
