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
//! Nothing here shells out to `gh`, reads `~/.config/gh` or a keyring, or makes
//! a network call: every config dir is a temp dir, every `hosts.yml` is written
//! by a fixture, and the #8510 token lookup and `GET /user` are table fakes.
//! Test: itself.

use std::path::Path;

use super::{pinned_gh_env_in, pinned_gh_env_with};
use crate::core::gh_account_dir::AccountDirSources;
use crate::core::gh_account_dir::gh_account_dir_tests::{TableCheck, TableProbe, migrated_dir};
use crate::core::gh_account_proof::AccountProver;
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

/// A registered record with no `gh_account` and no `github` section is not a
/// pin, and falls through unchanged.
/// Test: itself.
#[test]
fn a_record_pinning_nothing_is_not_a_pin() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    write_registry(
        registry_dir.path(),
        &format!(
            r#"{{"projects":{{"jev-matching":{{"name":"jev-matching","repo_url":"{ORIGIN}","default_branch":"main"}}}}}}"#
        ),
    );
    assert_eq!(
        pinned_gh_env_in(registry_dir.path(), ORIGIN).expect("readable"),
        None,
        "a record that pins nothing must fall through"
    );
}

/// 🔴 #5850 REGRESSION: an unpinned record must not shadow a pinned one for
/// the same repository.
///
/// Why this is the assertion: the lookup used to take the FIRST matching record
/// in name order. `Jev-Matching` sorts before `jev-matching`, so the unpinned
/// one won, the lookup answered "nothing pinned", and the probe ran as the
/// machine's global account.
/// Test: itself.
#[test]
fn an_unpinned_record_does_not_shadow_a_pinned_one() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let config_dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(config_dir.path(), "bob-duetto");
    write_registry(
        registry_dir.path(),
        &format!(
            r#"{{"projects":{{
                "Jev-Matching":{{"name":"Jev-Matching","repo_url":"{ORIGIN}.git","default_branch":"main"}},
                "jev-matching":{{"name":"jev-matching","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"bob-duetto","github":{{"config_dir":"{}"}}}}
            }}}}"#,
            config_dir.path().display()
        ),
    );
    let env = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect("readable")
        .expect("the pinned record must win over the unpinned one");
    assert_eq!(
        value_of(&env, "GH_CONFIG_DIR"),
        config_dir.path().to_string_lossy()
    );
}

/// 🔴 FAIL-CLOSED: two records for one repository pinning DIFFERENT identities
/// block, naming both records.
///
/// Why: picking one by position would probe as an account the operator may not
/// have meant; there is no order in which that choice is right.
/// Test: itself.
#[test]
fn two_disagreeing_pins_for_one_repository_fail_closed() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    write_registry(
        registry_dir.path(),
        &format!(
            r#"{{"projects":{{
                "a-jev":{{"name":"a-jev","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"bobmatnyc"}},
                "b-jev":{{"name":"b-jev","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"bob-duetto"}}
            }}}}"#
        ),
    );
    let err = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect_err("disagreeing pins must refuse, never pick one");
    assert!(
        err.contains("a-jev") && err.contains("b-jev") && err.contains("refusing to probe"),
        "the refusal must name both records; got: {err}"
    );
}

/// 🔴 FAIL-CLOSED: a MATCHING record that does not parse blocks even when
/// another matching record carries a usable pin.
///
/// Why: the broken record may pin a different account; "the other one pins" is
/// not an answer to that.
/// Test: itself.
#[test]
fn a_malformed_second_matching_record_fails_closed() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let config_dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(config_dir.path(), "bob-duetto");
    write_registry(
        registry_dir.path(),
        &format!(
            r#"{{"projects":{{
                "a-jev":{{"name":"a-jev","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"bob-duetto","github":{{"config_dir":"{}"}}}},
                "b-jev":{{"name":"b-jev","repo_url":"{ORIGIN}","default_branch":"main","gh_account":42}}
            }}}}"#,
            config_dir.path().display()
        ),
    );
    let err = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect_err("an unparsable matching record must refuse");
    assert!(
        err.contains("b-jev"),
        "the refusal must name the record; got: {err}"
    );
}

/// 🔴 #5850 REGRESSION: two records pinning the SAME login agree, and the one
/// carrying a `config_dir` wins.
///
/// Why this shape: `seed_from_config` writes `jev` with only `gh_account`, and
/// `tm <url> --user bob-duetto` later writes `jev-matching` with the same login
/// plus a scoped `config_dir`. Comparing whole records called that a conflict
/// and refused a repository that has one unambiguous identity.
/// Test: itself.
#[test]
fn same_login_pins_prefer_the_one_with_a_config_dir() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let config_dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(config_dir.path(), "bob-duetto");
    write_registry(
        registry_dir.path(),
        &format!(
            r#"{{"projects":{{
                "jev":{{"name":"jev","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"bob-duetto"}},
                "jev-matching":{{"name":"jev-matching","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"bob-duetto","github":{{"config_dir":"{}"}}}}
            }}}}"#,
            config_dir.path().display()
        ),
    );
    let env = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect("one login is not a disagreement")
        .expect("the config_dir pin must resolve");
    assert_eq!(
        value_of(&env, "GH_CONFIG_DIR"),
        config_dir.path().to_string_lossy()
    );
}

/// 🔴 #5850 REGRESSION: logins that differ only in case name one account.
///
/// Why: GitHub logins are case-insensitive, so `Bob-Duetto` and `bob-duetto`
/// are the same identity and must not block the repository.
/// Test: itself.
#[test]
fn same_login_in_a_different_case_agrees() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let config_dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(config_dir.path(), "bob-duetto");
    write_registry(
        registry_dir.path(),
        &format!(
            r#"{{"projects":{{
                "a-jev":{{"name":"a-jev","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"Bob-Duetto","github":{{"config_dir":"{0}"}}}},
                "b-jev":{{"name":"b-jev","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"bob-duetto","github":{{"config_dir":"{0}"}}}}
            }}}}"#,
            config_dir.path().display()
        ),
    );
    let env = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect("a case difference is not a disagreement")
        .expect("the pin must resolve");
    assert_eq!(
        value_of(&env, "GH_CONFIG_DIR"),
        config_dir.path().to_string_lossy()
    );
}

/// 🔴 FAIL-CLOSED: one login pinned through two DIFFERENT config dirs blocks.
///
/// Why: the two dirs may hold different credentials, so which one `gh` should
/// read is unanswered.
/// Test: itself.
#[test]
fn same_login_with_conflicting_config_dirs_fails_closed() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    write_registry(
        registry_dir.path(),
        &format!(
            r#"{{"projects":{{
                "a-jev":{{"name":"a-jev","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"bob-duetto","github":{{"config_dir":"/tmp/gh-one"}}}},
                "b-jev":{{"name":"b-jev","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"bob-duetto","github":{{"config_dir":"/tmp/gh-two"}}}}
            }}}}"#
        ),
    );
    let err = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect_err("two config dirs for one repository must refuse");
    assert!(
        err.contains("a-jev") && err.contains("b-jev") && err.contains("refusing to probe"),
        "got: {err}"
    );
}

/// 🔴 #5850 REGRESSION: a `config_dir` pin with NO login agrees with a login
/// pin on the same dir, and the chosen pin inherits that login.
///
/// Why: `seed_from_config` and `tm projects register --gh-config-dir` both write
/// a `github: {config_dir}` record with no login. Treating "no login" as a
/// login of its own refused the repository ("different gh accounts") and
/// spawned the session as the global account.
/// Test: itself.
#[test]
fn a_no_login_pin_agrees_with_a_login_pin_on_the_same_config_dir() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let config_dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(config_dir.path(), "bob-duetto");
    write_registry(
        registry_dir.path(),
        &format!(
            r#"{{"projects":{{
                "jev":{{"name":"jev","repo_url":"{ORIGIN}","default_branch":"main","github":{{"config_dir":"{0}"}}}},
                "widget":{{"name":"widget","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"bob-duetto","github":{{"config_dir":"{0}","account":"bob-duetto"}}}}
            }}}}"#,
            config_dir.path().display()
        ),
    );
    let pin = super::read_pin(registry_dir.path(), ORIGIN)
        .expect("a missing login is not a disagreement")
        .expect("the pins must resolve");
    assert_eq!(
        pin.account.as_deref(),
        Some("bob-duetto"),
        "the login must be inherited"
    );
    let env = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect("a missing login is not a disagreement")
        .expect("the pin must resolve");
    assert_eq!(
        value_of(&env, "GH_CONFIG_DIR"),
        config_dir.path().to_string_lossy()
    );
}

/// 🔴 FAIL-CLOSED: a no-login `config_dir` pin next to a login pin with a
/// DIFFERENT dir still blocks — leaving the login out never skips the dir check.
/// Test: itself.
#[test]
fn a_no_login_pin_with_a_different_config_dir_fails_closed() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    write_registry(
        registry_dir.path(),
        &format!(
            r#"{{"projects":{{
                "jev":{{"name":"jev","repo_url":"{ORIGIN}","default_branch":"main","github":{{"config_dir":"/tmp/gh-one"}}}},
                "widget":{{"name":"widget","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"bob-duetto","github":{{"config_dir":"/tmp/gh-two"}}}}
            }}}}"#
        ),
    );
    let err =
        pinned_gh_env_in(registry_dir.path(), ORIGIN).expect_err("two config dirs must refuse");
    assert!(
        err.contains("different gh config dirs") && !err.contains("different gh accounts"),
        "the refusal must name the real conflict; got: {err}"
    );
}

/// A host-only record next to a pinned duplicate does not block the pin.
///
/// Why: a host-only binding names no identity. Counting it as a pin would make
/// it "disagree" with the real one and block the repository.
/// Test: itself.
#[test]
fn a_host_only_duplicate_does_not_block_the_pinned_record() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let config_dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(config_dir.path(), "bob-duetto");
    write_registry(
        registry_dir.path(),
        &format!(
            r#"{{"projects":{{
                "a-jev":{{"name":"a-jev","repo_url":"{ORIGIN}","default_branch":"main","github":{{"host":"github.com"}}}},
                "b-jev":{{"name":"b-jev","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"bob-duetto","github":{{"config_dir":"{}"}}}}
            }}}}"#,
            config_dir.path().display()
        ),
    );
    let env = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect("a host-only duplicate must not block")
        .expect("the pinned record must resolve");
    assert_eq!(
        value_of(&env, "GH_CONFIG_DIR"),
        config_dir.path().to_string_lossy()
    );
}

/// A record that sets only `github.host` names no identity, and falls through.
///
/// Why: `GH_HOST` alone selects no account, so returning it as a pin would stop
/// the static tier from ever being consulted for that repository.
/// Test: itself.
#[test]
fn a_host_only_binding_is_not_a_pin() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    write_registry(
        registry_dir.path(),
        &format!(
            r#"{{"projects":{{"jev-matching":{{"name":"jev-matching","repo_url":"{ORIGIN}","default_branch":"main","github":{{"host":"github.example.com"}}}}}}}}"#
        ),
    );
    assert_eq!(
        pinned_gh_env_in(registry_dir.path(), ORIGIN).expect("readable"),
        None
    );
}

/// 🔴 FAIL-CLOSED: a registry document with no `projects` key blocks.
///
/// Why: `ProjectStore` always writes the key, so `{}` is not a registry this
/// process understands — reading it as "no pin" is the fallback.
/// Test: itself.
#[test]
fn a_registry_without_a_projects_key_fails_closed() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    write_registry(registry_dir.path(), "{}");
    let err = pinned_gh_env_in(registry_dir.path(), ORIGIN)
        .expect_err("a document without `projects` must refuse");
    assert!(
        err.contains("did not parse") && err.contains("refusing to probe"),
        "got: {err}"
    );
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
    let dir = tempfile::tempdir().expect("tempdir");
    let registry_dir = tempfile::tempdir().expect("tempdir");
    write_registry(registry_dir.path(), "{ this is not json");
    let failure = resolve_daemon_gh_env_in(dir.path(), ORIGIN, registry_dir.path())
        .expect_err("a registry refusal must block the gh spawn, never fall back");
    assert!(
        failure.to_string().contains("refusing to probe"),
        "the caller must surface the refusal; got: {failure}"
    );
}

/// The daemon's `gh` spawn runs inside the registry-pinned config dir, keyed by
/// the repository the caller passes. `dir` is not a git checkout, so this also
/// proves the lookup no longer re-reads `origin` from git (#5850).
/// Test: itself.
#[test]
fn daemon_gh_env_uses_the_registry_pin() {
    let dir = tempfile::tempdir().expect("tempdir");
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
    let env = resolve_daemon_gh_env_in(
        dir.path(),
        "duettoresearch/jev-matching",
        registry_dir.path(),
    )
    .expect("a usable pin must resolve");
    assert_eq!(
        value_of(&env, "GH_CONFIG_DIR"),
        config_dir.path().to_string_lossy()
    );
}

// ── #8510: an account-only pin uses only a token `GET /user` proves ─────────

/// A registry whose one record pins `account` with no `github:` binding.
fn write_account_only_registry(registry_dir: &Path, account: &str) {
    write_pin_registry(registry_dir, ORIGIN, account, "null");
}

/// A registry whose one record for `origin` pins `account` plus `github`
/// (raw JSON).
fn write_pin_registry(registry_dir: &Path, origin: &str, account: &str, github: &str) {
    write_registry(
        registry_dir,
        &format!(
            r#"{{"projects":{{"jev-matching":{{"name":"jev-matching","repo_url":"{origin}","default_branch":"main","gh_account":"{account}","github":{github}}}}}}}"#
        ),
    );
}

/// Sources naming only a static candidate dir.
fn static_only(dir: &Path) -> AccountDirSources {
    AccountDirSources {
        static_config_dir: Some(dir.to_path_buf()),
        ..AccountDirSources::default()
    }
}

/// Resolve `origin`'s registry pin with table fakes.
fn resolve_with(
    registry_dir: &Path,
    origin: &str,
    sources: &AccountDirSources,
    probe: &TableProbe,
    check: &TableCheck,
) -> Result<Option<crate::core::gh_identity::GhEnv>, String> {
    let prover = AccountProver {
        sources,
        probe,
        check,
        cache: None,
    };
    pinned_gh_env_with(registry_dir, origin, &prover)
}

/// A probe and check that prove `tok-octo-pinned` under `dir` on github.com.
fn proving(dir: &Path) -> (TableProbe, TableCheck) {
    (
        TableProbe::default().answer(dir, "github.com", "octo-pinned", Ok("tok-octo-pinned")),
        TableCheck::default().answer(
            "https://api.github.com",
            "tok-octo-pinned",
            Ok("octo-pinned"),
        ),
    )
}

/// 🔴 #8510 REGRESSION: a registry record pinning an account with no
/// `config_dir` resolves to the token proven under the static binding's dir,
/// injected as the token itself — never as `GH_CONFIG_DIR`.
///
/// Why: #8416 put the registry ahead of the static config, so this record
/// refused every merged-PR lookup even though the operator's static config
/// binds a dir holding the pinned account's token.
/// Test: itself.
#[tokio::test]
async fn an_account_only_pin_uses_a_token_proven_under_the_static_dir() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let static_dir = tempfile::tempdir().expect("tempdir");
    let static_dir = migrated_dir(static_dir.path());
    let registry = ProjectRegistry::load(registry_dir.path())
        .await
        .expect("load");
    registry
        .register(project("jev-matching", ORIGIN, Some("octo-pinned")))
        .await
        .expect("register");
    let (probe, check) = proving(&static_dir);

    let env = resolve_with(
        registry_dir.path(),
        ORIGIN,
        &static_only(&static_dir),
        &probe,
        &check,
    )
    .expect("a proven token must resolve the account-only pin")
    .expect("the pin must yield an identity");
    assert_eq!(value_of(&env, "GH_TOKEN"), "tok-octo-pinned");
    assert_eq!(
        value_of(&env, "GH_ENTERPRISE_TOKEN"),
        crate::core::gh_account::REFUSED_GH_TOKEN
    );
    assert!(
        !env.vars().iter().any(|(k, _)| k == "GH_CONFIG_DIR"),
        "a config dir is re-read against the keyring on every call: {}",
        env.describe()
    );
    assert!(
        env.unset_vars().iter().any(|k| k == "GH_CONFIG_DIR"),
        "an inherited GH_CONFIG_DIR must be cleared; got {:?}",
        env.unset_vars()
    );
}

/// With no static binding, tm's own `gh-accounts/<login>` dir is a candidate.
/// Test: itself.
#[test]
fn an_account_only_pin_falls_back_to_tms_own_account_dir() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let state_root = tempfile::tempdir().expect("tempdir");
    let account_dir = migrated_dir(&state_root.path().join("gh-accounts").join("octo-pinned"));
    write_account_only_registry(registry_dir.path(), "octo-pinned");
    let sources = AccountDirSources {
        state_root: Some(state_root.path().to_path_buf()),
        ..AccountDirSources::default()
    };
    let (probe, check) = proving(&account_dir);
    let env = resolve_with(registry_dir.path(), ORIGIN, &sources, &probe, &check)
        .expect("a proven tm account dir token must resolve")
        .expect("the pin must yield an identity");
    assert_eq!(value_of(&env, "GH_TOKEN"), "tok-octo-pinned");
}

/// 🔴 #8510 CRITICAL: a token that `GET /user` says is another account's is
/// refused, and the refusal names the account and the fix command.
/// Test: itself.
#[test]
fn an_account_only_pin_refuses_a_token_for_another_account() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let static_dir = tempfile::tempdir().expect("tempdir");
    let static_dir = migrated_dir(static_dir.path());
    write_account_only_registry(registry_dir.path(), "octo-pinned");
    let probe =
        TableProbe::default().answer(&static_dir, "github.com", "octo-pinned", Ok("tok-global"));
    let check =
        TableCheck::default().answer("https://api.github.com", "tok-global", Ok("octo-other"));
    let err = resolve_with(
        registry_dir.path(),
        ORIGIN,
        &static_only(&static_dir),
        &probe,
        &check,
    )
    .expect_err("another account's token must refuse");
    assert!(
        err.contains("authenticates as 'octo-other'")
            && err.contains("refusing to probe")
            && !err.contains("tok-"),
        "got: {err}"
    );
    // #8510 r6: the whole rendered text, so a quoting slip such as
    // `'octo-pinned''s` cannot pass unseen.
    let shown = static_dir.display();
    assert_eq!(
        err,
        format!(
            "this repository is pinned to gh account 'octo-pinned' with no \
             `github.config_dir`, and a gh token is used for it only once `GET /user` proves \
             it belongs to 'octo-pinned' (#5851) — refusing to probe it as whichever account \
             is globally active. No candidate token is proven to belong to 'octo-pinned': \
             {shown}: the token gh returned for 'octo-pinned' authenticates as 'octo-other'. \
             Pin a gh config dir logged in as 'octo-pinned': `tm projects register \
             jev-matching --repo-url {ORIGIN} --gh-account octo-pinned --gh-config-dir \
             <dir>` (#8510)."
        )
    );
}

/// 🔴 #8510 network identity check: when `GET /user` cannot confirm the token
/// (timeout, HTTP error), the daemon lookup refuses. It never falls through to
/// the static tier (`Ok(None)`) and never uses the candidate dir.
/// Test: itself.
#[test]
fn an_account_only_pin_refuses_when_the_user_check_fails() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let static_dir = tempfile::tempdir().expect("tempdir");
    let static_dir = migrated_dir(static_dir.path());
    write_account_only_registry(registry_dir.path(), "octo-pinned");
    let probe =
        TableProbe::default().answer(&static_dir, "github.com", "octo-pinned", Ok("tok-bob"));
    let failure = "GET https://api.github.com/user did not answer in time";
    let check = TableCheck::default().answer("https://api.github.com", "tok-bob", Err(failure));
    let err = resolve_with(
        registry_dir.path(),
        ORIGIN,
        &static_only(&static_dir),
        &probe,
        &check,
    )
    .expect_err("an unconfirmed token must refuse, not fall through");
    assert!(
        err.contains("could not be proven") && err.contains(failure) && !err.contains("tok-"),
        "got: {err}"
    );
    assert_eq!(
        check.calls(),
        vec![("https://api.github.com".to_string(), "tok-bob".to_string())],
        "the refusal must follow a network check of the candidate's token"
    );
}

/// 🔴 FAIL-CLOSED: a candidate dir that does not exist refuses by name.
/// Test: itself.
#[test]
fn an_account_only_pin_refuses_a_missing_candidate_dir() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let parent = tempfile::tempdir().expect("tempdir");
    let missing = parent.path().join("gh-never-created");
    write_account_only_registry(registry_dir.path(), "octo-pinned");
    let probe = TableProbe::default();
    let err = resolve_with(
        registry_dir.path(),
        ORIGIN,
        &static_only(&missing),
        &probe,
        &TableCheck::default(),
    )
    .expect_err("a missing dir must refuse");
    assert!(
        err.contains(&format!("{} does not exist", missing.display())),
        "got: {err}"
    );
    assert!(probe.calls().is_empty());
}

/// 🔴 #8510 HIGH: the daemon lookup never runs gh in a dir gh would migrate.
/// Test: itself.
#[test]
fn an_account_only_pin_never_runs_gh_in_an_unmigrated_dir() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let static_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        static_dir.path().join("config.yml"),
        "git_protocol: https\n",
    )
    .expect("cfg");
    write_account_only_registry(registry_dir.path(), "octo-pinned");
    let (probe, check) = proving(static_dir.path());
    let err = resolve_with(
        registry_dir.path(),
        ORIGIN,
        &static_only(static_dir.path()),
        &probe,
        &check,
    )
    .expect_err("an unmigrated dir must refuse");
    assert!(err.contains("declares no `version"), "got: {err}");
    assert!(probe.calls().is_empty(), "gh ran: {:?}", probe.calls());
}

/// 🔴 The repository OWNER never selects the account: a tm dir for the owner
/// `duettoresearch` does not answer a `octo-pinned` pin.
/// Test: itself.
#[test]
fn an_account_only_pin_never_resolves_from_the_repository_owner() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let state_root = tempfile::tempdir().expect("tempdir");
    let owner_dir = migrated_dir(&state_root.path().join("gh-accounts").join("duettoresearch"));
    write_account_only_registry(registry_dir.path(), "octo-pinned");
    let sources = AccountDirSources {
        state_root: Some(state_root.path().to_path_buf()),
        ..AccountDirSources::default()
    };
    let probe =
        TableProbe::default().answer(&owner_dir, "github.com", "octo-pinned", Ok("tok-owner"));
    let err = resolve_with(
        registry_dir.path(),
        ORIGIN,
        &sources,
        &probe,
        &TableCheck::default(),
    )
    .expect_err("the owner's dir must never stand in for the pinned account");
    assert!(
        err.contains("gh-accounts/octo-pinned does not exist"),
        "got: {err}"
    );
}

/// 🔴 FAIL-CLOSED: a pinned login that is not a safe path segment never joins
/// onto the tm state root.
/// Test: itself.
#[test]
fn an_account_only_pin_refuses_an_unsafe_login_segment() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let state_root = tempfile::tempdir().expect("tempdir");
    write_account_only_registry(registry_dir.path(), "..");
    let sources = AccountDirSources {
        state_root: Some(state_root.path().to_path_buf()),
        ..AccountDirSources::default()
    };
    let err = resolve_with(
        registry_dir.path(),
        ORIGIN,
        &sources,
        &TableProbe::default(),
        &TableCheck::default(),
    )
    .expect_err("an unsafe login must refuse");
    assert!(err.contains("is not a valid account login"), "got: {err}");
}

/// The static candidate is this origin's own `projects[].github` binding; the
/// global `github:` binding is not "for this origin" and is never asked.
/// Test: itself.
#[test]
fn account_dir_sources_take_only_this_origins_static_binding() {
    use crate::core::trusty_tools_config::{ProjectConfig, TrustyToolsConfig};
    let binding = |dir: &str| GithubConfig {
        config_dir: Some(dir.into()),
        ..GithubConfig::default()
    };
    let config = TrustyToolsConfig {
        github: Some(binding("/cfg/global")),
        projects: vec![ProjectConfig {
            name: "jev-matching".into(),
            repo_url: format!("{ORIGIN}.git"),
            github: Some(binding("/cfg/gh-octo-other")),
            ..ProjectConfig::default()
        }],
        ..TrustyToolsConfig::default()
    };
    let root = std::path::PathBuf::from("/state");
    let own = Some(std::path::PathBuf::from("/home/me/.config/gh"));
    let matched = AccountDirSources::for_origin(&config, ORIGIN, root.clone(), own.clone());
    assert_eq!(
        matched.static_config_dir.as_deref(),
        Some(Path::new("/cfg/gh-octo-other"))
    );
    assert_eq!(matched.state_root.as_deref(), Some(root.as_path()));
    assert_eq!(matched.own_config_dir, own);
    let other =
        AccountDirSources::for_origin(&config, "https://github.com/acme/widget", root, None);
    assert_eq!(other.static_config_dir, None);
}

/// 🔴 #8510: a `-u` lookup that finds no token for the login refuses, and
/// never echoes a token.
/// Test: itself.
#[test]
fn an_account_only_pin_refuses_a_dir_whose_login_has_no_token() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let static_dir = tempfile::tempdir().expect("tempdir");
    let static_dir = migrated_dir(static_dir.path());
    write_account_only_registry(registry_dir.path(), "octo-pinned");
    let probe = TableProbe::default().answer(
        &static_dir,
        "github.com",
        "octo-pinned",
        Err("exit status 1: no oauth token found for github.com account octo-pinned"),
    );
    let err = resolve_with(
        registry_dir.path(),
        ORIGIN,
        &static_only(&static_dir),
        &probe,
        &TableCheck::default(),
    )
    .expect_err("no token must refuse");
    assert!(
        err.contains("-u octo-pinned` failed") && !err.contains("tok-"),
        "got: {err}"
    );
}

/// 🔴 #8510 HIGH: an Enterprise Server pin's proven token rides
/// `GH_ENTERPRISE_TOKEN`, the variable gh reads for that host; `GH_TOKEN` gets
/// the nobody-token.
/// Test: itself.
#[test]
fn an_account_only_pin_on_an_enterprise_server_uses_gh_enterprise_token() {
    let ghe_origin = "https://ghe.corp/duettoresearch/jev-matching";
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let static_dir = tempfile::tempdir().expect("tempdir");
    let static_dir = migrated_dir(static_dir.path());
    write_pin_registry(registry_dir.path(), ghe_origin, "octo-pinned", "null");
    let probe = TableProbe::default().answer(&static_dir, "ghe.corp", "octo-pinned", Ok("tok-ghe"));
    let check =
        TableCheck::default().answer("https://ghe.corp/api/v3", "tok-ghe", Ok("octo-pinned"));
    let env = resolve_with(
        registry_dir.path(),
        ghe_origin,
        &static_only(&static_dir),
        &probe,
        &check,
    )
    .expect("a proven Enterprise Server token resolves")
    .expect("the pin yields an identity");
    assert_eq!(value_of(&env, "GH_ENTERPRISE_TOKEN"), "tok-ghe");
    assert_eq!(
        value_of(&env, "GH_TOKEN"),
        crate::core::gh_account::REFUSED_GH_TOKEN
    );
}

/// 🔴 #8510 r4: an account-only pin never sets a `GH_HOST` other than the host
/// its token was proven on. A matching `github.host` (in any case) is kept as
/// the proven host; a different one refuses and names both hosts.
/// Test: itself.
#[test]
fn an_account_only_pin_refuses_a_gh_host_other_than_the_proven_one() {
    let static_dir = tempfile::tempdir().expect("tempdir");
    let static_dir = migrated_dir(static_dir.path());

    let registry_dir = tempfile::tempdir().expect("tempdir");
    write_pin_registry(
        registry_dir.path(),
        ORIGIN,
        "octo-pinned",
        r#"{"host":"GitHub.com"}"#,
    );
    let (probe, check) = proving(&static_dir);
    let env = resolve_with(
        registry_dir.path(),
        ORIGIN,
        &static_only(&static_dir),
        &probe,
        &check,
    )
    .expect("a host naming the proven one resolves")
    .expect("the pin yields an identity");
    assert_eq!(value_of(&env, "GH_HOST"), "github.com");

    let registry_dir = tempfile::tempdir().expect("tempdir");
    write_pin_registry(
        registry_dir.path(),
        ORIGIN,
        "octo-pinned",
        r#"{"host":"ghe.corp"}"#,
    );
    let (probe, check) = proving(&static_dir);
    let err = resolve_with(
        registry_dir.path(),
        ORIGIN,
        &static_only(&static_dir),
        &probe,
        &check,
    )
    .expect_err("a GH_HOST the token was not proven on must refuse");
    assert!(
        err.contains("'ghe.corp'") && err.contains("'github.com'") && !err.contains("tok-"),
        "got: {err}"
    );
}

/// 🔴 FAIL-CLOSED: an origin whose host cannot be read proves nothing.
/// Test: itself.
#[test]
fn an_account_only_pin_refuses_an_origin_with_no_host() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let static_dir = tempfile::tempdir().expect("tempdir");
    let static_dir = migrated_dir(static_dir.path());
    write_pin_registry(registry_dir.path(), "local-checkout", "octo-pinned", "null");
    let (probe, check) = proving(&static_dir);
    let err = resolve_with(
        registry_dir.path(),
        "local-checkout",
        &static_only(&static_dir),
        &probe,
        &check,
    )
    .expect_err("no host, no token");
    assert!(
        err.contains("cannot tell which gh host serves it"),
        "got: {err}"
    );
}

/// 🔴 A pinned `config_dir` is used as-is; no candidate is ever asked.
/// Test: itself.
#[test]
fn a_pinned_config_dir_is_never_replaced_by_a_borrowed_one() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let pinned_dir = tempfile::tempdir().expect("tempdir");
    let borrow_dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(pinned_dir.path(), "octo-pinned");
    let borrow_dir = migrated_dir(borrow_dir.path());
    write_pin_registry(
        registry_dir.path(),
        ORIGIN,
        "octo-pinned",
        &format!(r#"{{"config_dir":"{}"}}"#, pinned_dir.path().display()),
    );
    let (probe, check) = proving(&borrow_dir);
    let env = resolve_with(
        registry_dir.path(),
        ORIGIN,
        &static_only(&borrow_dir),
        &probe,
        &check,
    )
    .expect("a pinned dir resolves")
    .expect("the pin yields an identity");
    assert_eq!(
        value_of(&env, "GH_CONFIG_DIR"),
        pinned_dir.path().to_string_lossy()
    );
    assert!(probe.calls().is_empty(), "gh ran: {:?}", probe.calls());
}

/// 🔴 A `token_env` pin whose variable is unset refuses with the token_env
/// reason; it never looks for a candidate token instead.
/// Test: itself.
#[test]
fn a_token_env_pin_never_borrows_a_config_dir() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let borrow_dir = tempfile::tempdir().expect("tempdir");
    let borrow_dir = migrated_dir(borrow_dir.path());
    write_pin_registry(
        registry_dir.path(),
        ORIGIN,
        "octo-pinned",
        r#"{"token_env":"TM_8510_NEVER_SET_TOKEN_VAR"}"#,
    );
    let (probe, check) = proving(&borrow_dir);
    let err = resolve_with(
        registry_dir.path(),
        ORIGIN,
        &static_only(&borrow_dir),
        &probe,
        &check,
    )
    .expect_err("an unset token_env must refuse, never borrow");
    assert!(
        err.contains("`github.token_env` 'TM_8510_NEVER_SET_TOKEN_VAR'"),
        "got: {err}"
    );
    assert!(probe.calls().is_empty(), "gh ran: {:?}", probe.calls());
}

/// 🔴 A record whose `gh_account` and `github.account` differ refuses by name.
/// Test: itself.
#[test]
fn a_record_naming_two_accounts_fails_closed() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(dir.path(), "octo-pinned");
    write_pin_registry(
        registry_dir.path(),
        ORIGIN,
        "octo-pinned",
        &format!(
            r#"{{"account":"octo-other","config_dir":"{}"}}"#,
            dir.path().display()
        ),
    );
    let err = resolve_with(
        registry_dir.path(),
        ORIGIN,
        &AccountDirSources::default(),
        &TableProbe::default(),
        &TableCheck::default(),
    )
    .expect_err("two accounts on one record must refuse");
    assert!(
        err.contains("gh_account 'octo-pinned' but its `github.account` is 'octo-other'"),
        "got: {err}"
    );
}

/// 🔴 FAIL-CLOSED: a symlinked `gh-accounts/<login>` dir is never asked.
/// Test: itself.
#[cfg(unix)]
#[test]
fn an_account_only_pin_refuses_a_symlinked_tm_account_dir() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let state_root = tempfile::tempdir().expect("tempdir");
    let real = tempfile::tempdir().expect("tempdir");
    let real = migrated_dir(real.path());
    std::fs::create_dir_all(state_root.path().join("gh-accounts")).expect("gh-accounts");
    let link = state_root.path().join("gh-accounts").join("octo-pinned");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    write_account_only_registry(registry_dir.path(), "octo-pinned");
    let sources = AccountDirSources {
        state_root: Some(state_root.path().to_path_buf()),
        ..AccountDirSources::default()
    };
    let (probe, check) = proving(&link);
    let err = resolve_with(registry_dir.path(), ORIGIN, &sources, &probe, &check)
        .expect_err("a symlinked account dir must refuse");
    assert!(err.contains("is a symlink"), "got: {err}");
    assert!(probe.calls().is_empty(), "gh ran: {:?}", probe.calls());
}
