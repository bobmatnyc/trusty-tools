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

use super::{AccountDirSources, pinned_gh_env_in, pinned_gh_env_with};
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

// ── #8510: an account-only pin borrows a VERIFIED config dir ────────────────

/// A `hosts.yml` whose active user is `active` and whose `users:` map lists
/// every login in `listed`.
fn write_hosts_yml_listing(config_dir: &Path, active: &str, listed: &[&str]) {
    std::fs::create_dir_all(config_dir).expect("config dir");
    let users: String = listed
        .iter()
        .map(|login| format!("        {login}:\n            git_protocol: https\n"))
        .collect();
    std::fs::write(
        config_dir.join("hosts.yml"),
        format!("github.com:\n    users:\n{users}    git_protocol: https\n    user: {active}\n"),
    )
    .expect("hosts.yml");
}

/// A registry whose one record pins `account` with no `github:` binding.
fn write_account_only_registry(registry_dir: &Path, account: &str) {
    write_registry(
        registry_dir,
        &format!(
            r#"{{"projects":{{"jev-matching":{{"name":"jev-matching","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"{account}","github":null}}}}}}"#
        ),
    );
}

/// Resolve the account-only `bob-duetto` pin against one static candidate dir
/// and an EMPTY tm state root, expecting a refusal.
fn refusal_with_static_dir(static_dir: &Path) -> String {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let state_root = tempfile::tempdir().expect("tempdir");
    write_account_only_registry(registry_dir.path(), "bob-duetto");
    let sources = AccountDirSources {
        static_config_dir: Some(static_dir.to_path_buf()),
        state_root: Some(state_root.path().to_path_buf()),
    };
    pinned_gh_env_with(registry_dir.path(), ORIGIN, &sources)
        .expect_err("an unverified candidate must refuse, never guess")
}

/// 🔴 #8510 REGRESSION: a registry record pinning an account with no
/// `config_dir` resolves through the static binding for its origin when that
/// dir's ACTIVE user is the pinned account.
///
/// Why: #8416 put the registry ahead of the static config, so this record
/// refused every merged-PR lookup even though the operator's static config
/// binds a dir that selects the pinned account.
/// Test: itself.
#[tokio::test]
async fn an_account_only_pin_borrows_a_static_dir_whose_active_user_matches() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let static_dir = tempfile::tempdir().expect("tempdir");
    let state_root = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(static_dir.path(), "bob-duetto");
    let registry = ProjectRegistry::load(registry_dir.path())
        .await
        .expect("load");
    registry
        .register(project("jev-matching", ORIGIN, Some("bob-duetto")))
        .await
        .expect("register");
    let sources = AccountDirSources {
        static_config_dir: Some(static_dir.path().to_path_buf()),
        state_root: Some(state_root.path().to_path_buf()),
    };

    let env = pinned_gh_env_with(registry_dir.path(), ORIGIN, &sources)
        .expect("a verified static dir must resolve the account-only pin")
        .expect("the pin must yield an identity");
    assert_eq!(
        value_of(&env, "GH_CONFIG_DIR"),
        static_dir.path().to_string_lossy()
    );
    assert!(
        env.unset_vars().iter().any(|k| k == "GH_TOKEN"),
        "an inherited GH_TOKEN must not outrank the borrowed dir; got {:?}",
        env.unset_vars()
    );
}

/// With no static binding, tm's own `gh-accounts/<login>` dir is the candidate.
/// Test: itself.
#[test]
fn an_account_only_pin_falls_back_to_tms_own_account_dir() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let state_root = tempfile::tempdir().expect("tempdir");
    let account_dir = state_root.path().join("gh-accounts").join("bob-duetto");
    write_hosts_yml(&account_dir, "bob-duetto");
    write_account_only_registry(registry_dir.path(), "bob-duetto");
    let sources = AccountDirSources {
        static_config_dir: None,
        state_root: Some(state_root.path().to_path_buf()),
    };
    let env = pinned_gh_env_with(registry_dir.path(), ORIGIN, &sources)
        .expect("a verified tm account dir must resolve")
        .expect("the pin must yield an identity");
    assert_eq!(
        value_of(&env, "GH_CONFIG_DIR"),
        account_dir.to_string_lossy()
    );
}

/// 🔴 A static binding whose active user is a DIFFERENT account refuses, and
/// the refusal names the dir, the active user, and the fix command.
/// Test: itself.
#[test]
fn an_account_only_pin_refuses_a_static_dir_active_as_another_account() {
    let static_dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(static_dir.path(), "bobmatnyc");
    let err = refusal_with_static_dir(static_dir.path());
    assert!(
        err.contains("is active as 'bobmatnyc'") && err.contains("refusing to probe"),
        "got: {err}"
    );
    assert!(
        err.contains(&format!(
            "tm projects register jev-matching --repo-url {ORIGIN} --gh-account bob-duetto \
             --gh-config-dir <dir>"
        )),
        "the refusal must name the fix command; got: {err}"
    );
}

/// 🔴 A dir that LISTS the pinned account but is active as another refuses:
/// `gh` under that dir acts as the active user, not as every listed one.
/// Test: itself.
#[test]
fn an_account_only_pin_refuses_a_dir_listing_it_but_active_as_another() {
    let static_dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml_listing(static_dir.path(), "bobmatnyc", &["bob-duetto", "bobmatnyc"]);
    let err = refusal_with_static_dir(static_dir.path());
    assert!(
        err.contains("lists 'bob-duetto' but is active as 'bobmatnyc'"),
        "got: {err}"
    );
}

/// 🔴 FAIL-CLOSED: a candidate dir that does not exist refuses by name.
/// Test: itself.
#[test]
fn an_account_only_pin_refuses_a_missing_candidate_dir() {
    let parent = tempfile::tempdir().expect("tempdir");
    let missing = parent.path().join("gh-never-created");
    let err = refusal_with_static_dir(&missing);
    assert!(
        err.contains(&format!("{} does not exist", missing.display())),
        "got: {err}"
    );
}

/// 🔴 FAIL-CLOSED: an unreadable `hosts.yml` refuses. It is a DIRECTORY, so
/// the read fails for any user, root included.
/// Test: itself.
#[test]
fn an_account_only_pin_refuses_an_unreadable_hosts_yml() {
    let static_dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(static_dir.path().join("hosts.yml")).expect("blocker");
    let err = refusal_with_static_dir(static_dir.path());
    assert!(err.contains("could not be read"), "got: {err}");
}

/// 🔴 FAIL-CLOSED: a `hosts.yml` that is not YAML refuses.
/// Test: itself.
#[test]
fn an_account_only_pin_refuses_a_malformed_hosts_yml() {
    let static_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        static_dir.path().join("hosts.yml"),
        "github.com: [unclosed\n",
    )
    .expect("hosts.yml");
    let err = refusal_with_static_dir(static_dir.path());
    assert!(err.contains("did not parse"), "got: {err}");
}

/// 🔴 FAIL-CLOSED: a `hosts.yml` naming no active `user:` refuses, even when
/// its `users:` map lists the pinned account.
/// Test: itself.
#[test]
fn an_account_only_pin_refuses_a_hosts_yml_with_no_active_user() {
    let static_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        static_dir.path().join("hosts.yml"),
        "github.com:\n    users:\n        bob-duetto:\n            git_protocol: https\n",
    )
    .expect("hosts.yml");
    let err = refusal_with_static_dir(static_dir.path());
    assert!(
        err.contains("names no active github.com user"),
        "got: {err}"
    );
}

/// 🔴 The repository OWNER never selects the account: a tm dir for the owner
/// `duettoresearch`, active as that owner, does not answer a `bob-duetto` pin.
/// Test: itself.
#[test]
fn an_account_only_pin_never_resolves_from_the_repository_owner() {
    let registry_dir = tempfile::tempdir().expect("tempdir");
    let state_root = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(
        &state_root.path().join("gh-accounts").join("duettoresearch"),
        "duettoresearch",
    );
    write_account_only_registry(registry_dir.path(), "bob-duetto");
    let sources = AccountDirSources {
        static_config_dir: None,
        state_root: Some(state_root.path().to_path_buf()),
    };
    let err = pinned_gh_env_with(registry_dir.path(), ORIGIN, &sources)
        .expect_err("the owner's dir must never stand in for the pinned account");
    assert!(
        err.contains("gh-accounts/bob-duetto does not exist"),
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
        static_config_dir: None,
        state_root: Some(state_root.path().to_path_buf()),
    };
    let err = pinned_gh_env_with(registry_dir.path(), ORIGIN, &sources)
        .expect_err("an unsafe login must refuse");
    assert!(
        err.contains("not a valid gh-accounts path segment"),
        "got: {err}"
    );
}

/// The static candidate is this origin's own `projects[].github` binding; the
/// global `github:` binding is not "for this origin" and is never borrowed.
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
            github: Some(binding("/cfg/gh-bobmatnyc")),
            ..ProjectConfig::default()
        }],
        ..TrustyToolsConfig::default()
    };
    let root = std::path::PathBuf::from("/state");
    let matched = AccountDirSources::for_origin(&config, ORIGIN, root.clone());
    assert_eq!(
        matched.static_config_dir.as_deref(),
        Some(Path::new("/cfg/gh-bobmatnyc"))
    );
    assert_eq!(matched.state_root.as_deref(), Some(root.as_path()));
    let other = AccountDirSources::for_origin(&config, "https://github.com/acme/widget", root);
    assert_eq!(other.static_config_dir, None);
}
