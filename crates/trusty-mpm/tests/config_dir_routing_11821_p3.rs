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

/// Points `HOME` at a scratch dir, clears [`PATH_VARS`], restores all on drop.
///
/// `env_serial` runs one test at a time, so no other test observes the rewrite.
struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn install(home: &Path) -> Self {
        let saved = std::iter::once("HOME")
            .chain(PATH_VARS)
            .map(|k| (k, std::env::var_os(k)))
            .collect();
        // SAFETY: `env_serial` forces one test thread; Drop restores every var.
        unsafe {
            std::env::set_var("HOME", home);
            for k in PATH_VARS {
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
        std::fs::create_dir_all(home.join(".trusty-tools")).expect("mkdir .trusty-tools");
        Self {
            _tmp: tmp,
            home,
            cfg: root.join("cfg"),
        }
    }

    fn write_install_file(&self, body: &str) {
        std::fs::write(self.home.join(".trusty-tools/install.toml"), body).expect("install.toml");
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
