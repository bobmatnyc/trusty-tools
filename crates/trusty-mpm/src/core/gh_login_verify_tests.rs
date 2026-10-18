//! Tests for the #9750 launch-time login check. Every gh and `GET /user`
//! answer comes from a table fake; no test runs gh, reads a keyring or sends a
//! request.

use std::path::{Path, PathBuf};

use super::{
    VerifySeams, check_pin, launch_refusal, pin_sources, session_pin_with, verify_pinned_login,
};
use crate::core::gh_account::PinnedGhIdentity;
use crate::core::gh_account_dir::AccountDirSources;
use crate::core::gh_account_dir::gh_account_dir_tests::{TableCheck, TableProbe, migrated_dir};
use crate::core::gh_org_accounts::{OrgAccounts, OrgAccountsError};
use crate::core::trusty_tools_config::{GithubConfig, TrustyToolsConfig};
use crate::project::record::Project;
use crate::session_manager::ssh_host_alias::SshHostAliases;

/// A planted token: no refusal, message or log may ever contain it.
const SENTINEL: &str = "ghp_9750SentinelTokenNeverPrinted";

/// The repository every pin below is for.
const ORIGIN: &str = "https://github.com/acme-9750/widget";

/// Sources holding only `dir`, as the operator's own gh config dir.
fn only(dir: &Path) -> AccountDirSources {
    AccountDirSources {
        own_config_dir: Some(dir.to_path_buf()),
        ..Default::default()
    }
}

/// A migrated candidate dir whose `--user octo` lookup answers `token`, and a
/// `GET /user` that answers `who` for the sentinel.
fn fixture(
    token: Result<&str, &str>,
    who: Result<&str, &str>,
) -> (tempfile::TempDir, PathBuf, TableProbe, TableCheck) {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = migrated_dir(&root.path().join("gh"));
    let probe = TableProbe::default().answer(&dir, "github.com", "octo", token);
    let check = TableCheck::default().answer("https://api.github.com", SENTINEL, who);
    (root, dir, probe, check)
}

/// #9750: a keyring answer `GET /user` proves is the pinned login's passes,
/// and names the dir the shim will ask.
#[test]
fn a_matching_login_is_verified_and_names_its_dir() {
    let (_root, dir, probe, check) = fixture(Ok(SENTINEL), Ok("OCTO"));
    let verified = verify_pinned_login(&only(&dir), "octo", "github.com", &probe, &check)
        .expect("a matching login verifies");
    assert_eq!(verified.login, "octo");
    assert_eq!(verified.host, "github.com");
    assert_eq!(verified.config_dir, dir);
}

/// 🔴 #9750 / #5851: the keyring returned another account's token for
/// `--user octo`; the launch must not trust it.
#[test]
fn another_accounts_token_is_refused() {
    let (_root, dir, probe, check) = fixture(Ok(SENTINEL), Ok("octo-other"));
    let err = verify_pinned_login(&only(&dir), "octo", "github.com", &probe, &check)
        .expect_err("another account's token is no proof");
    assert!(err.contains("authenticates as 'octo-other'"), "{err}");
}

/// #9750: no keyring login for the account — gh printed nothing.
#[test]
fn an_empty_token_is_refused() {
    let (_root, dir, probe, check) = fixture(Err("printed no token"), Ok("octo"));
    let err = verify_pinned_login(&only(&dir), "octo", "github.com", &probe, &check)
        .expect_err("no token is no proof");
    assert!(err.contains("printed no token"), "{err}");
    assert!(check.calls().is_empty(), "GET /user ran without a token");
}

/// #9750: gh is absent or hangs on a locked keychain — the bounded lookup
/// fails, and so does the launch.
#[test]
fn a_gh_timeout_is_refused() {
    for failure in [
        "timed out after 5s",
        "No such file or directory (os error 2)",
    ] {
        let (_root, dir, probe, check) = fixture(Err(failure), Ok("octo"));
        let err = verify_pinned_login(&only(&dir), "octo", "github.com", &probe, &check)
            .expect_err("a failed lookup is no proof");
        assert!(err.contains(failure), "{err}");
    }
}

/// #9750: the token was found but GitHub did not confirm it (non-200).
#[test]
fn a_failed_user_check_is_refused() {
    let failed = "GET https://api.github.com/user answered HTTP 401";
    let (_root, dir, probe, check) = fixture(Ok(SENTINEL), Err(failed));
    let err = verify_pinned_login(&only(&dir), "octo", "github.com", &probe, &check)
        .expect_err("an unconfirmed token is no proof");
    assert!(err.contains("HTTP 401"), "{err}");
}

/// 🔴 #9750: no refusal and no launch message carries the planted token.
#[test]
fn no_refusal_ever_contains_the_token() {
    for who in [Ok("octo-other"), Err("answered HTTP 500")] {
        let (_root, dir, probe, check) = fixture(Ok(SENTINEL), who);
        let err = verify_pinned_login(&only(&dir), "octo", "github.com", &probe, &check)
            .expect_err("refused");
        assert!(
            !err.contains(SENTINEL),
            "a token reached the refusal: {err}"
        );
        let message = launch_refusal("octo", "github.com", &err);
        assert!(!message.contains(SENTINEL), "{message}");
    }
}

/// #9750: the refusal text is the plan of record's, word for word.
#[test]
fn the_launch_refusal_is_the_plans_exact_message() {
    assert_eq!(
        launch_refusal("octo", "ghe.corp", "gh exited 1"),
        "tm: cannot start session: gh account 'octo' is not logged in as that account on \
         ghe.corp (gh exited 1). Run: gh auth login --hostname ghe.corp (as octo) or gh auth \
         refresh -h ghe.corp -u octo. No token was injected."
    );
}

/// #8914 HIGH 3: a pin's own config dir is asked first, as the spawn asks it.
#[test]
fn a_config_dir_pin_is_asked_before_any_other_dir() {
    let pinned = PinnedGhIdentity {
        account: Some("octo".into()),
        config_dir: Some(PathBuf::from("/pinned/gh")),
        ..Default::default()
    };
    let sources = pin_sources(
        &pinned,
        ORIGIN,
        Path::new("/state"),
        &TrustyToolsConfig::default(),
        Some(PathBuf::from("/home/me/.config/gh")),
    );
    let first = sources.candidates("octo").into_iter().next();
    assert_eq!(first, Some(Ok(PathBuf::from("/pinned/gh"))));
}

/// The launch check's seams over one candidate dir and no static config.
fn seams<'a>(
    dir: &Path,
    config: &'a TrustyToolsConfig,
    aliases: &'a SshHostAliases,
    probe: &'a TableProbe,
    check: &'a TableCheck,
) -> VerifySeams<'a> {
    VerifySeams {
        state_root: Path::new("/nonexistent-9750-state"),
        aliases,
        config,
        own_config_dir: Some(dir.to_path_buf()),
        probe,
        check,
    }
}

/// An account-only `octo` pin.
fn octo_pin() -> PinnedGhIdentity {
    PinnedGhIdentity {
        account: Some("octo".into()),
        ..Default::default()
    }
}

/// #9750 Q1: a pin whose login the keyring proves passes the launch check.
#[test]
fn a_verified_pin_passes_the_launch_check() {
    let (_root, dir, probe, check) = fixture(Ok(SENTINEL), Ok("octo"));
    let (config, aliases) = (TrustyToolsConfig::default(), SshHostAliases::empty());
    let verified = check_pin(
        &octo_pin(),
        ORIGIN,
        &seams(&dir, &config, &aliases, &probe, &check),
    )
    .expect("passes")
    .expect("a pinned login is verified");
    assert_eq!(verified.config_dir, dir);
}

/// 🔴 #9750 Q1: an interactive launch whose pinned login is not in the keyring
/// is refused with the plan's message, naming no token.
#[test]
fn an_unverified_pin_is_refused_with_the_exact_message() {
    let (_root, dir, probe, check) = fixture(Ok(SENTINEL), Ok("octo-other"));
    let (config, aliases) = (TrustyToolsConfig::default(), SshHostAliases::empty());
    let err = check_pin(
        &octo_pin(),
        ORIGIN,
        &seams(&dir, &config, &aliases, &probe, &check),
    )
    .expect_err("refused");
    assert!(
        err.starts_with(
            "tm: cannot start session: gh account 'octo' is not logged in as that account on \
             github.com ("
        ) && err.ends_with(
            "). Run: gh auth login --hostname github.com (as octo) or gh auth refresh -h \
             github.com -u octo. No token was injected."
        ),
        "{err}"
    );
    assert!(!err.contains(SENTINEL), "{err}");
}

/// FAIL-CLOSED: a config dir pin whose `hosts.yml` names no login cannot be
/// verified, so the launch is refused without asking gh.
#[test]
fn a_config_dir_pin_naming_no_login_refuses() {
    let (_root, dir, probe, check) = fixture(Ok(SENTINEL), Ok("octo"));
    let empty = tempfile::tempdir().expect("tempdir");
    let pinned = PinnedGhIdentity {
        config_dir: Some(empty.path().to_path_buf()),
        ..Default::default()
    };
    let (config, aliases) = (TrustyToolsConfig::default(), SshHostAliases::empty());
    let err = check_pin(
        &pinned,
        ORIGIN,
        &seams(&dir, &config, &aliases, &probe, &check),
    )
    .expect_err("refused");
    assert!(err.contains("names no account for github.com"), "{err}");
    assert!(probe.calls().is_empty(), "gh ran for a pin naming no login");
}

/// 🔴 #9750: a pin whose `account` is blank refuses the in-place relaunch with
/// the launch check's own words — it never relaunches with the pane's token.
#[test]
fn a_blank_account_pin_refuses_the_relaunch_as_check_pin_does() {
    let (_root, dir, probe, check) = fixture(Ok(SENTINEL), Ok("octo"));
    let pinned = PinnedGhIdentity {
        account: Some("   ".into()),
        ..Default::default()
    };
    let (config, aliases) = (TrustyToolsConfig::default(), SshHostAliases::empty());
    let launch = check_pin(
        &pinned,
        ORIGIN,
        &seams(&dir, &config, &aliases, &probe, &check),
    )
    .expect_err("the launch check refuses");
    let relaunch = crate::core::gh_session_shim::pinned_relaunch_env_with(
        &pinned,
        ORIGIN,
        Path::new("/nonexistent-9750-state"),
        &aliases,
        |_| Err("no launch may run for a blank account".into()),
    )
    .expect_err("the relaunch refuses");
    assert_eq!(relaunch, launch);
    assert!(probe.calls().is_empty(), "gh ran for a blank account");
}

/// 🔴 #9750: the interactive launch check refuses a checkout whose origin git
/// cannot read, instead of treating it as unpinned.
#[cfg(unix)]
#[test]
fn a_launch_check_whose_origin_git_cannot_read_is_refused() {
    use crate::core::gh_session_shim::gh_session_shim_tests::{
        restore_checkout, unreadable_checkout,
    };
    let checkout = unreadable_checkout();
    let result = super::check_launch_login(checkout.path(), None);
    restore_checkout(checkout.path());
    let err = result.expect_err("refused");
    assert!(err.contains("git cannot read the origin"), "{err}");
}

/// #8510 r4: an SSH alias no `~/.ssh/config` entry renames names no host; the
/// launch is refused before gh runs.
#[test]
fn a_pin_on_an_unresolved_ssh_alias_refuses_before_gh_runs() {
    let (_root, dir, probe, check) = fixture(Ok(SENTINEL), Ok("octo"));
    let (config, aliases) = (TrustyToolsConfig::default(), SshHostAliases::empty());
    let err = check_pin(
        &octo_pin(),
        "git@gh-work:acme-9750/widget.git",
        &seams(&dir, &config, &aliases, &probe, &check),
    )
    .expect_err("refused");
    assert!(err.contains("'gh-work'") && err.ends_with("No token was injected."));
    assert!(probe.calls().is_empty(), "gh ran with no host");
}

/// Write a registry holding `projects` the way `ProjectStore` publishes it.
fn registry_with(projects: &[Project]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let map: serde_json::Map<String, serde_json::Value> = projects
        .iter()
        .map(|p| {
            (
                p.name.clone(),
                serde_json::to_value(p).expect("project json"),
            )
        })
        .collect();
    let body = serde_json::json!({ "projects": map });
    std::fs::write(dir.path().join("projects.json"), body.to_string()).expect("projects.json");
    dir
}

/// A minimal registry record for `ORIGIN`.
fn record(gh_account: Option<&str>, github: Option<GithubConfig>) -> Project {
    Project {
        name: "widget".into(),
        repo_url: ORIGIN.into(),
        default_branch: "main".into(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        gh_account: gh_account.map(str::to_string),
        github,
        commit_name: None,
        commit_email: None,
        worktree: None,
    }
}

/// An `[accounts]` loader that must never be asked.
fn no_map() -> Result<OrgAccounts, OrgAccountsError> {
    panic!("a registry pin must not read the [accounts] table")
}

/// An `[accounts]` loader with no entries.
fn empty_map() -> Result<OrgAccounts, OrgAccountsError> {
    OrgAccounts::from_toml("", Path::new("/home/u/.trusty-mpm/config.toml"))
}

/// T2 (#8383 kept): an origin nobody pins is unpinned, so no shim is added.
#[test]
fn an_unregistered_origin_is_unpinned() {
    let registry = registry_with(&[]);
    let pin = session_pin_with(ORIGIN, registry.path(), &SshHostAliases::empty(), empty_map);
    assert_eq!(pin, Ok(None));
}

/// #9750: the CLI finds the registry pin the daemon's spawn uses.
#[test]
fn a_registry_pin_is_found_for_its_origin() {
    let registry = registry_with(&[record(Some("octo"), None)]);
    let pin = session_pin_with(ORIGIN, registry.path(), &SshHostAliases::empty(), no_map)
        .expect("readable")
        .expect("pinned");
    assert_eq!(pin.account.as_deref(), Some("octo"));
}

/// #9091: an origin no record pins takes its org's `[accounts]` login.
#[test]
fn an_org_mapped_origin_is_pinned_to_the_mapped_login() {
    let registry = registry_with(&[]);
    let load = || {
        OrgAccounts::from_toml(
            "[accounts]\nacme-9750 = \"octo-mapped\"\n",
            Path::new("/home/u/.trusty-mpm/config.toml"),
        )
    };
    let pin = session_pin_with(ORIGIN, registry.path(), &SshHostAliases::empty(), load)
        .expect("readable")
        .expect("pinned");
    assert_eq!(pin.account.as_deref(), Some("octo-mapped"));
}

/// #9091 r2: a `token_env`-only record is a pin that names no login and no
/// dir; the daemon injects nothing for it, so the launch check skips it.
#[test]
fn a_token_env_only_record_is_unpinned() {
    let github = GithubConfig {
        token_env: Some("TM_9750_TOKEN".into()),
        ..Default::default()
    };
    let registry = registry_with(&[record(None, Some(github))]);
    let pin = session_pin_with(ORIGIN, registry.path(), &SshHostAliases::empty(), no_map);
    assert_eq!(pin, Ok(None));
}

/// An `[accounts]` loader whose table is named but does not parse.
fn broken_map() -> Result<OrgAccounts, OrgAccountsError> {
    OrgAccounts::from_toml(
        "[accounts]\nacme-9750 = \n",
        Path::new("/home/u/.trusty-mpm/config.toml"),
    )
}

/// #9750: an unreadable registry or a broken `[accounts]` table refuses, never
/// reads as unpinned, and the refusal never echoes a token in the origin.
#[test]
fn an_unreadable_registry_or_broken_org_map_refuses_the_launch() {
    type Load = fn() -> Result<OrgAccounts, OrgAccountsError>;
    let origin = format!("https://x-access-token:{SENTINEL}@github.com/acme-9750/widget");
    let rows: [(&str, Option<&str>, Load, &str); 2] = [
        (
            "unreadable registry",
            Some("{not json"),
            no_map,
            "did not parse",
        ),
        ("broken org map", None, broken_map, "[accounts]"),
    ];
    for (label, registry_text, load, needle) in rows {
        let registry = registry_with(&[]);
        if let Some(text) = registry_text {
            std::fs::write(registry.path().join("projects.json"), text).expect("projects.json");
        }
        let err = session_pin_with(&origin, registry.path(), &SshHostAliases::empty(), load)
            .expect_err(label);
        assert!(err.contains(needle), "{label}: {err}");
        assert!(!err.contains(SENTINEL), "{label} leaked the token: {err}");
    }
}
