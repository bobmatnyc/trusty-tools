//! Tests for the registry-backed daemon `gh` identity lookup (#5850).
//!
//! Why: this module decides whether a housekeeping `gh pr list` runs as the
//! account a project is PINNED to or as whoever the machine's global gh config
//! names, so every arm needs its own assertion — and the failure arms need one
//! most, because a fail-OPEN there is silent by construction: the probe
//! succeeds, answers about the wrong account's view of the world, and the
//! survey reports "no pull request found".
//!
//! The happy path writes the registry through the REAL
//! [`crate::project::ProjectRegistry`] write path, so the sync reader under
//! test is proven against the bytes the operator-facing pinning paths actually
//! produce, not against a hand-written fixture that could drift from them. The
//! failure arms write `projects.json` by hand, because a real registry cannot
//! produce a corrupt one on demand.
//!
//! Nothing here shells out to `gh` or reads `~/.config/gh`: every config dir is
//! a temp dir and every `hosts.yml` is written by a fixture.
//! Test: itself.

use std::path::Path;

use super::{RegistryPin, pinned_gh_env_in};
use crate::core::trusty_tools_config::GithubConfig;
use crate::project::ProjectRegistry;
use crate::project::record::Project;
use crate::session_manager::worktree_reclaim_gh::resolve_daemon_gh_env_in;

/// A repository only the pinned account can see.
const ORIGIN: &str = "https://github.com/duettoresearch/jev-matching";

/// Build a minimal `Project` fixture, mirroring the register-route shape.
fn project(name: &str, repo_url: &str, gh_account: Option<&str>) -> Project {
    Project {
        name: name.to_string(),
        repo_url: repo_url.to_string(),
        default_branch: "main".to_string(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        gh_account: gh_account.map(str::to_string),
        github: None,
        commit_name: None,
        commit_email: None,
        worktree: None,
    }
}

/// A `hosts.yml` naming one logged-in github.com account.
fn write_hosts_yml(config_dir: &Path, account: &str) {
    std::fs::create_dir_all(config_dir).expect("config dir");
    std::fs::write(
        config_dir.join("hosts.yml"),
        format!("github.com:\n    git_protocol: https\n    user: {account}\n"),
    )
    .expect("hosts.yml");
}

/// Write a `projects.json` verbatim — the shape a real registry publishes.
fn write_registry(registry_dir: &Path, body: &str) {
    std::fs::create_dir_all(registry_dir).expect("registry dir");
    std::fs::write(registry_dir.join("projects.json"), body).expect("projects.json");
}

/// The value of one resolved override.
fn value_of(env: &crate::core::gh_identity::GhEnv, key: &str) -> String {
    env.vars()
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| panic!("{key} not set; got {:?}", env.vars()))
}

/// 🔴 #5850 REGRESSION: the pin `tm --user <login> <url>` persists must reach a
/// daemon-side `gh` spawn.
///
/// Why this is the assertion: before this module the daemon read only the
/// STATIC `TrustyToolsConfig` `projects:` list, which the registry write path
/// never touches — so this exact registration resolved to an EMPTY binding and
/// `gh pr list` ran as the machine's global account. Registering through the
/// real `ProjectRegistry` and reading it back synchronously is what proves the
/// two halves meet.
/// Test: itself.
#[tokio::test]
async fn registry_pin_resolves_the_projects_scoped_config_dir() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let config_dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(config_dir.path(), "bob-duetto");

    let registry = ProjectRegistry::load(registry_dir.path())
        .await
        .expect("load");
    registry
        .register(Project {
            github: Some(GithubConfig {
                config_dir: Some(config_dir.path().to_path_buf()),
                ..GithubConfig::default()
            }),
            ..project("jev-matching", ORIGIN, Some("bob-duetto"))
        })
        .await
        .expect("register");

    let env = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect("a readable registry must not fail")
        .expect("the registered pin must resolve");
    assert_eq!(
        value_of(&env, "GH_CONFIG_DIR"),
        config_dir.path().to_string_lossy(),
        "the daemon must spawn gh inside the project's own scoped config dir"
    );
    // #6668: the binding is only real if an inherited token cannot outrank it.
    assert!(
        env.unset_vars().iter().any(|k| k == "GH_TOKEN"),
        "an inherited GH_TOKEN must be cleared; got {:?}",
        env.unset_vars()
    );
}

/// A repository no registered project names is not pinned, and falls through.
///
/// Why: this is the ONLY outcome allowed to reach the static-config fallback.
/// If it ever became an `Err`, every unregistered worktree on the host would
/// block instead of resolving the way it did before #5850.
/// Test: itself.
#[tokio::test]
async fn registry_pin_is_absent_for_an_unregistered_origin() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(registry_dir.path())
        .await
        .expect("load");
    registry
        .register(project("widget", "https://github.com/acme/widget", None))
        .await
        .expect("register");

    assert_eq!(
        pinned_gh_env_in(registry_dir.path(), ORIGIN).expect("readable"),
        None,
        "an unregistered origin pins nothing and must fall through"
    );
}

/// A host that has registered nothing pins nothing.
///
/// Why: an absent `projects.json` is the state of every fresh install, and
/// folding it in with an unreadable one would turn first-run into a hard
/// failure for every worktree.
/// Test: itself.
#[test]
fn an_absent_registry_file_is_not_a_pin() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        pinned_gh_env_in(registry_dir.path(), ORIGIN).expect("absent is not an error"),
        None
    );
}

/// 🔴 FAIL-CLOSED arm 1: a registry that cannot be READ blocks.
///
/// Why: an I/O failure leaves "does this repository pin an account?"
/// unanswered, and the wrong answer is not visible — `gh` would succeed as the
/// global account and report a private repository as having no pull requests.
/// `projects.json` is created as a DIRECTORY rather than chmod-ed, so the
/// failure is deterministic for any user, root included.
/// Test: itself.
#[test]
fn an_unreadable_registry_fails_closed() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(registry_dir.path().join("projects.json")).expect("blocker");
    let err = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect_err("an unreadable registry must refuse, never fall back");
    assert!(
        err.contains("refusing to probe"),
        "the refusal must say it refused; got: {err}"
    );
}

/// 🔴 FAIL-CLOSED arm 2a: a registry DOCUMENT that does not parse blocks.
/// Test: itself.
#[test]
fn a_malformed_registry_document_fails_closed() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    write_registry(registry_dir.path(), "{ this is not json");
    let err = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect_err("an unparsable registry must refuse, never fall back");
    assert!(
        err.contains("did not parse") && err.contains("refusing to probe"),
        "got: {err}"
    );
}

/// 🔴 FAIL-CLOSED arm 2b: a MATCHING record that does not parse blocks, and the
/// refusal names the record.
///
/// Why: a record whose `gh_account` is not a string could carry a pin this
/// process cannot read. Treating it as "unpinned" is the silent substitution.
/// Test: itself.
#[test]
fn a_malformed_matching_record_fails_closed() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    write_registry(
        registry_dir.path(),
        &format!(
            r#"{{"projects":{{"jev-matching":{{"name":"jev-matching","repo_url":"{ORIGIN}","default_branch":"main","gh_account":42}}}}}}"#
        ),
    );
    let err = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect_err("an unparsable matching record must refuse");
    assert!(
        err.contains("jev-matching") && err.contains("refusing to probe"),
        "the refusal must name the record; got: {err}"
    );
}

/// One broken record must not block every OTHER project on the host.
///
/// Why: 261 worktrees share one `projects.json`. Parsing the whole map
/// strictly would let a single corrupt entry suspend pull-request lookups for
/// the entire registry — a much worse outage than the one #5850 fixes.
/// Test: itself.
#[test]
fn a_malformed_unrelated_record_does_not_block_a_match() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let config_dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(config_dir.path(), "bob-duetto");
    write_registry(
        registry_dir.path(),
        &format!(
            r#"{{"projects":{{
                "broken":{{"repo_url":"https://github.com/acme/widget","gh_account":42}},
                "jev-matching":{{"name":"jev-matching","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"bob-duetto","github":{{"config_dir":"{}"}}}}
            }}}}"#,
            config_dir.path().display()
        ),
    );
    let env = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect("a sibling's corruption must not block this repository")
        .expect("the pin must resolve");
    assert_eq!(
        value_of(&env, "GH_CONFIG_DIR"),
        config_dir.path().to_string_lossy()
    );
}

/// 🔴 FAIL-CLOSED arm 3: a pinned config dir holding NO credential blocks, and
/// the refusal names the account.
///
/// Why: this is the shape `auto_persist_account_selection` leaves behind when
/// `gh auth login` has not been run inside the scoped dir yet. Falling back to
/// the global account here is the #5851 wrong-identity defect, restated for the
/// daemon's housekeeping path.
/// Test: itself.
#[test]
fn a_pinned_config_dir_without_a_credential_fails_closed() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let config_dir = tempfile::tempdir().expect("tempdir");
    write_registry(
        registry_dir.path(),
        &format!(
            r#"{{"projects":{{"jev-matching":{{"name":"jev-matching","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"bob-duetto","github":{{"config_dir":"{}"}}}}}}}}"#,
            config_dir.path().display()
        ),
    );
    let err = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect_err("an empty config dir must refuse, never fall back");
    assert!(
        err.contains("bob-duetto") && err.contains("refusing to fall back"),
        "the refusal must name the pinned account; got: {err}"
    );
}

/// 🔴 FAIL-CLOSED arm 3b: an account pinned with NO config dir blocks.
///
/// Why (#5851): `gh auth token -u <account>` returns the globally-active
/// account's token on a keyring-backed host, so honouring an account-only pin
/// by minting a token is exactly the wrong-user probe this change prevents.
/// Test: itself.
#[tokio::test]
async fn an_account_only_pin_fails_closed_naming_the_account() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(registry_dir.path())
        .await
        .expect("load");
    registry
        .register(project("jev-matching", ORIGIN, Some("bob-duetto")))
        .await
        .expect("register");

    let err = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect_err("an account-only pin must refuse, never fall back");
    assert!(
        err.contains("bob-duetto") && err.contains("refusing to probe"),
        "the refusal must name the pinned account; got: {err}"
    );
}

/// 🔴 FAIL-CLOSED arm 3c: a `token_env` pin whose variable is unset blocks.
///
/// Why: `resolve_gh_env` skips a `token_env` it cannot read, so the binding
/// resolves to NO identity. Reading that as "nothing pinned" hands the probe to
/// the static config and then the machine's global account.
/// Test: itself.
#[test]
fn an_unset_token_env_pin_fails_closed() {
    const VAR: &str = "TRUSTY_MPM_TEST_5850_UNSET_TOKEN_ENV";
    assert!(std::env::var_os(VAR).is_none(), "{VAR} must stay unset");
    let registry_dir = tempfile::tempdir().expect("tempdir");
    write_registry(
        registry_dir.path(),
        &format!(
            r#"{{"projects":{{"jev-matching":{{"name":"jev-matching","repo_url":"{ORIGIN}","default_branch":"main","github":{{"token_env":"{VAR}"}}}}}}}}"#
        ),
    );
    let err = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect_err("an unreadable token_env pin must refuse, never fall back");
    assert!(
        err.contains(VAR) && err.contains("refusing to fall back"),
        "the refusal must name the variable; got: {err}"
    );
}

/// A record that pins nothing is not a pin, and falls through unchanged.
/// Test: itself.
#[test]
fn a_record_pinning_nothing_is_not_a_pin() {
    assert!(RegistryPin::default().is_empty());
}

/// A git checkout whose `origin` is `origin` — what a daemon `gh` spawn holds.
fn checkout_with_origin(origin: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for args in [vec!["init", "-q"], vec!["remote", "add", "origin", origin]] {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(&args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?} failed");
    }
    dir
}

/// 🔴 FAIL-CLOSED arm 4: a registry refusal reaches the daemon's `gh` spawn as
/// a [`GhFailure`], never as the static/ambient identity.
///
/// Why: every arm above returns an `Err`, but the refusal only protects the
/// operator if `resolve_daemon_gh_env` propagates it. Swallowing it there
/// would fall through to `TrustyToolsConfig` and the machine's global account.
/// Test: itself.
#[test]
fn daemon_gh_env_refuses_when_the_registry_cannot_answer() {
    let checkout = checkout_with_origin(ORIGIN);
    let registry_dir = tempfile::tempdir().expect("tempdir");
    write_registry(registry_dir.path(), "{ this is not json");
    let failure = resolve_daemon_gh_env_in(checkout.path(), registry_dir.path())
        .expect_err("a registry refusal must block the gh spawn, never fall back");
    assert!(
        failure.to_string().contains("refusing to probe"),
        "the caller must surface the refusal; got: {failure}"
    );
}

/// The daemon's `gh` spawn runs inside the registry-pinned config dir.
/// Test: itself.
#[test]
fn daemon_gh_env_uses_the_registry_pin() {
    let checkout = checkout_with_origin(ORIGIN);
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let config_dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(config_dir.path(), "bob-duetto");
    write_registry(
        registry_dir.path(),
        &format!(
            r#"{{"projects":{{"jev-matching":{{"name":"jev-matching","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"bob-duetto","github":{{"config_dir":"{}"}}}}}}}}"#,
            config_dir.path().display()
        ),
    );
    let env = resolve_daemon_gh_env_in(checkout.path(), registry_dir.path())
        .expect("a usable pin must resolve");
    assert_eq!(
        value_of(&env, "GH_CONFIG_DIR"),
        config_dir.path().to_string_lossy()
    );
}
