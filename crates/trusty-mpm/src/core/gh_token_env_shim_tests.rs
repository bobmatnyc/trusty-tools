//! Tests for routing a `github.token_env` binding through the gh shim
//! (#10487). No test runs the real gh, reads a real keyring or sends a
//! request: [`TableCheck`] answers `GET /user` and [`TableKeyring`] is the
//! keyring and its read-back.

use std::path::{Path, PathBuf};

use super::{TokenEnvSeams, shim_token_env_with};
use crate::core::gh_account_dir::gh_account_dir_tests::TableCheck;
use crate::core::gh_identity::{GhEnv, resolve_gh_env};
use crate::core::gh_keyring_token::gh_keyring_token_tests::TableKeyring;
use crate::core::gh_session_shim::GH_SHIM_ENV;
use crate::core::trusty_tools_config::GithubConfig;
use trusty_common::credentials::test_sandbox::CredentialSandbox;

/// A planted token: no env value, message, shim or file may contain it.
const SENTINEL: &str = "ghp_10487TokenEnvSentinelNeverInEnv";
/// The `github.token_env` NAME the tests bind.
const SOURCE: &str = "TM_TEST_10487_TOKEN_ENV";

/// The `GH_TOKEN` binding `resolve_gh_env` builds for `token_env = SOURCE`.
fn token_env_binding(host: Option<&str>) -> GhEnv {
    let _g = crate::core::trusty_tools_config::env_test_lock();
    // SAFETY: guarded by env_test_lock; removed before the lock drops.
    unsafe { std::env::set_var(SOURCE, SENTINEL) };
    let cfg = GithubConfig {
        token_env: Some(SOURCE.to_string()),
        host: host.map(str::to_string),
        ..GithubConfig::default()
    };
    let env = resolve_gh_env(Some(&cfg));
    unsafe { std::env::remove_var(SOURCE) };
    env.expect("a token_env binding resolves")
}

/// A `GET /user` that names `login` for the sentinel on github.com.
fn check_as(login: &str) -> TableCheck {
    TableCheck::default().answer("https://api.github.com", SENTINEL, Ok(login))
}

/// The seams over `check`, `keyring` and `state_root`, with gh at `/bin/gh`.
fn seams<'a>(
    check: &'a TableCheck,
    keyring: &'a TableKeyring,
    state_root: &'a Path,
    real_gh: Result<PathBuf, String>,
) -> TokenEnvSeams<'a> {
    TokenEnvSeams {
        check,
        probe: keyring,
        keyring,
        real_gh,
        state_root,
    }
}

/// Every file under `dir`, read as text where it reads.
fn all_text_under(dir: &Path) -> String {
    let mut out = String::new();
    for entry in walkdir::WalkDir::new(dir).into_iter().flatten() {
        if entry.file_type().is_file() {
            out.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
        }
    }
    out
}

/// 🔴 #10487: the binding resolves to `GH_TOKEN`, and the launch env built
/// from it carries no token value and names the shim instead.
#[test]
#[serial_test::serial]
fn a_token_env_binding_reaches_the_launch_env_as_the_shim_not_a_token_10487() {
    let _sandbox = CredentialSandbox::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = token_env_binding(None);
    let (check, keyring) = (check_as("octo"), TableKeyring::default());
    let launch = shim_token_env_with(
        &env,
        seams(&check, &keyring, tmp.path(), Ok("/bin/gh".into())),
    )
    .expect("the binding is shimmed");
    for (name, value) in launch.vars() {
        assert!(!value.contains(SENTINEL), "{name} carries the token");
        assert!(!name.ends_with("TOKEN"), "{name} is set: {launch:?}");
    }
    for cleared in ["GH_TOKEN", "GITHUB_TOKEN", SOURCE] {
        assert!(
            launch.unset_vars().iter().any(|n| n == cleared),
            "{cleared} is not cleared: {launch:?}"
        );
    }
    let shim = launch.var(GH_SHIM_ENV).expect("the shim dir is named");
    assert!(!all_text_under(Path::new(shim)).contains(SENTINEL));
    assert_eq!(launch.var("GH_USER"), Some("octo"));
    // The session's gh resolves the token per call from this keyring entry.
    assert_eq!(
        keyring.token("github.com", "octo").as_deref(),
        Some(SENTINEL)
    );
    let hosts = std::fs::read_to_string(tmp.path().join("gh-accounts/octo/hosts.yml"))
        .expect("hosts.yml names the login");
    assert!(!hosts.contains(SENTINEL), "#10037: no token in hosts.yml");
}

/// #10487: a binding that sets no `GH_TOKEN` is not touched.
#[test]
fn a_binding_with_no_token_passes_through_unchanged_10487() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = GithubConfig {
        config_dir: Some(PathBuf::from("/cfg/dir")),
        ..GithubConfig::default()
    };
    let env = resolve_gh_env(Some(&cfg)).expect("resolves");
    let (check, keyring) = (TableCheck::default(), TableKeyring::default());
    let out = shim_token_env_with(
        &env,
        seams(&check, &keyring, tmp.path(), Ok("/bin/gh".into())),
    )
    .expect("passes through");
    assert_eq!(out, env);
    assert!(
        check.calls().is_empty(),
        "no GET /user for a tokenless binding"
    );
}

/// 🔴 #10487 review: the local-only clamp's empty `GH_TOKEN` is no
/// `token_env` binding; it passes through with no `GET /user`.
#[test]
fn a_local_only_env_passes_through_with_no_get_user_10487() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = GhEnv::local_only();
    let (check, keyring) = (TableCheck::default(), TableKeyring::default());
    let out = shim_token_env_with(
        &env,
        seams(&check, &keyring, tmp.path(), Ok("/bin/gh".into())),
    )
    .expect("a local-only env passes through");
    assert_eq!(out, env);
    assert!(check.calls().is_empty(), "local-only made a GET /user");
}

/// 🔴 #10487 Fail-Open Check: a keyring that refuses the write refuses the
/// launch; nothing hands the token to the env instead.
#[test]
fn a_token_env_keyring_failure_refuses_and_injects_no_token_10487() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = token_env_binding(None);
    let check = check_as("octo");
    let keyring = TableKeyring::failing(&format!("keychain locked near {SENTINEL}"));
    let err = shim_token_env_with(
        &env,
        seams(&check, &keyring, tmp.path(), Ok("/bin/gh".into())),
    )
    .expect_err("a keyring failure must refuse");
    assert!(err.contains("gh keyring"), "{err}");
    assert!(err.contains("No token was injected"), "{err}");
    assert!(
        !err.contains(SENTINEL),
        "the refusal leaks the token: {err}"
    );
    assert!(!all_text_under(tmp.path()).contains(SENTINEL));
}

/// 🔴 #10487: a token `GET /user` cannot prove refuses, and nothing is stored.
#[test]
fn an_unproven_token_env_token_refuses_and_stores_nothing_10487() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = token_env_binding(None);
    let (check, keyring) = (TableCheck::default(), TableKeyring::default());
    let err = shim_token_env_with(
        &env,
        seams(&check, &keyring, tmp.path(), Ok("/bin/gh".into())),
    )
    .expect_err("an unproven token must refuse");
    assert!(err.contains("GET /user"), "{err}");
    assert!(!err.contains(SENTINEL), "{err}");
    assert_eq!(keyring.token("github.com", "octo"), None);
}

/// 🔴 #10487: with no real gh to shim, the launch refuses rather than keep
/// the token in the env.
#[test]
fn a_token_env_launch_with_no_gh_refuses_10487() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = token_env_binding(None);
    let (check, keyring) = (check_as("octo"), TableKeyring::default());
    let no_gh = Err("gh is not installed on this PATH".to_string());
    let err = shim_token_env_with(&env, seams(&check, &keyring, tmp.path(), no_gh))
        .expect_err("no gh must refuse");
    assert!(err.contains("gh shim"), "{err}");
    assert!(!err.contains(SENTINEL), "{err}");
}

/// #10487: the binding names the variable its token came from, never a value.
#[test]
fn a_token_env_binding_names_its_source_variable_10487() {
    let env = token_env_binding(Some("ghe.example.com"));
    assert_eq!(env.token_source(), Some(SOURCE));
    assert!(!format!("{env:?}").contains(SENTINEL));
}

/// Is `name` a credential-bearing gh/GitHub token spelling (#10487)?
fn is_credential_name(name: &str) -> bool {
    name == SOURCE
        || (name.contains("TOKEN") && (name.starts_with("GH") || name.contains("GITHUB")))
}

/// 🔴 #10487: the WHOLE env a relaunched `claude` receives — tm's own env with
/// every token spelling planted, then the launch's sets and removals — carries
/// no `GH_TOKEN`, `GITHUB_TOKEN`, `GH_ENTERPRISE_TOKEN`,
/// `GITHUB_ENTERPRISE_TOKEN` or `token_env` variable with a secret value.
#[test]
#[serial_test::serial]
fn the_whole_launch_env_carries_no_gh_token_value_10487() {
    use std::collections::BTreeMap;
    let _sandbox = CredentialSandbox::enter();
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = token_env_binding(None);
    let (check, keyring) = (check_as("octo"), TableKeyring::default());
    let shimmed = shim_token_env_with(
        &env,
        seams(&check, &keyring, tmp.path(), Ok("/bin/gh".into())),
    )
    .expect("the binding is shimmed");
    // The same last step `relaunch_gh_env` runs, with a probe that never runs gh.
    let launch = shimmed.for_session_launch_with(None, |_, _| Ok(()));
    // Any program: only the env this launch builds is under test.
    let mut cmd = std::process::Command::new("launched-session");
    launch.apply_to(&mut cmd);

    // tm's own process env, as the launched child would inherit it.
    let mut child: BTreeMap<String, String> = [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
        SOURCE,
    ]
    .iter()
    .map(|n| ((*n).to_string(), SENTINEL.to_string()))
    .collect();
    for (name, value) in cmd.get_envs() {
        let name = name.to_string_lossy().into_owned();
        match value {
            Some(v) => child.insert(name, v.to_string_lossy().into_owned()),
            None => child.remove(&name),
        };
    }
    for (name, value) in &child {
        assert!(!value.contains(SENTINEL), "{name} carries the token");
        assert!(
            !is_credential_name(name),
            "{name} is set on the launch env: {launch:?}"
        );
    }
}
