//! Tests for the `[accounts]` org → account map (#9091).

use super::*;

fn parse(raw: &str) -> Result<OrgAccounts, OrgAccountsError> {
    OrgAccounts::from_toml(raw, Path::new("/home/u/.trusty-mpm/config.toml"))
}

fn map(raw: &str) -> OrgAccounts {
    parse(raw).expect("valid table")
}

/// 🔴 #9091: the owner's table parses, next to unrelated sections.
#[test]
fn accounts_table_parses() {
    let accounts = map(
        "[models]\ndefault = \"sonnet\"\n\n[accounts]\nduettoresearch = \"bob-duetto\"\nbobmatnyc = \"bobmatnyc\"\n",
    );
    assert_eq!(accounts.account_for("duettoresearch"), Some("bob-duetto"));
    assert_eq!(accounts.account_for("bobmatnyc"), Some("bobmatnyc"));
    assert_eq!(accounts.account_for("acme"), None);
}

#[test]
fn absent_accounts_table_is_empty() {
    assert_eq!(
        map("[models]\ndefault = \"sonnet\"\n"),
        OrgAccounts::default()
    );
    assert_eq!(map(""), OrgAccounts::default());
}

/// GitHub org names ignore case, so the lookup does too.
#[test]
fn org_lookup_ignores_case() {
    let accounts = map("[accounts]\nDuettoResearch = \"bob-duetto\"\n");
    for org in ["duettoresearch", "DUETTORESEARCH", "DuettoResearch"] {
        assert_eq!(accounts.account_for(org), Some("bob-duetto"), "{org}");
    }
}

#[test]
fn orgs_differing_only_in_case_are_refused() {
    let err = parse("[accounts]\nacme = \"a\"\nACME = \"b\"\n").expect_err("ambiguous");
    assert!(err.to_string().contains("twice"), "{err}");
}

/// 🔴 #9091 Fail-Open Check: every malformed table is an error naming the
/// problem, never an empty map that would run as the active account.
#[test]
fn a_malformed_accounts_table_is_an_error() {
    for (raw, needle) in [
        ("accounts = \"bob-duetto\"\n", "must be a table"),
        ("[accounts]\nduettoresearch = 1\n", "must be a string"),
        ("[accounts]\nduettoresearch = \"\"\n", "is not a gh login"),
        (
            "[accounts]\nduettoresearch = \"bob duetto\"\n",
            "is not a gh login",
        ),
        (
            "[accounts]\n\"duetto research\" = \"bob-duetto\"\n",
            "not a GitHub org",
        ),
    ] {
        let err = parse(raw).expect_err(raw);
        assert!(
            matches!(err, OrgAccountsError::Invalid { .. }),
            "{raw}: {err:?}"
        );
        assert!(err.to_string().contains(needle), "{raw}: {err}");
        assert!(err.to_string().contains("[accounts]"), "{raw}: {err}");
    }
}

/// 🔴 #9091 Fail-Open Check: the likeliest typo — an unquoted login — is a
/// TOML syntax error. In a file holding the table it must surface, not read as
/// "no mapping"; a header spelled with spaces and a comment still counts.
#[test]
fn a_syntax_error_with_an_accounts_header_is_an_error() {
    for raw in [
        "[accounts]\nduettoresearch = bob-duetto\n",
        "[models]\ndefault = sonnet\n\n  [ accounts ]  # work orgs\nacme = \"octo\"\n",
    ] {
        let err = parse(raw).expect_err(raw);
        assert!(matches!(err, OrgAccountsError::Parse { .. }), "{err:?}");
        assert!(err.to_string().contains("config.toml"), "{err}");
    }
}

/// #9091 (PM default): a syntax error in a file with no `[accounts]` header is
/// an empty table plus a warning, as `MpmConfig::load` reads it.
#[test]
fn a_syntax_error_without_an_accounts_header_is_an_empty_map() {
    for raw in [
        "[models]\ndefault = sonnet\n",
        "# [accounts]\n[models]\nx = y\n",
    ] {
        let (accounts, warning) =
            OrgAccounts::parse(raw, Path::new("/home/u/.trusty-mpm/config.toml")).expect(raw);
        assert_eq!(accounts, OrgAccounts::default(), "{raw}");
        let warning = warning.expect("a warning names the ignored error");
        assert!(warning.contains("config.toml"), "{warning}");
        assert_eq!(map(raw), OrgAccounts::default(), "{raw}");
    }
}

#[test]
fn load_reads_the_table_from_the_root() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("config.toml"),
        "[accounts]\nduettoresearch = \"bob-duetto\"\n",
    )
    .expect("write");
    let accounts = OrgAccounts::load(dir.path()).expect("loads");
    assert_eq!(accounts.account_for("duettoresearch"), Some("bob-duetto"));

    std::fs::write(dir.path().join("config.toml"), "[accounts]\nx = 1\n").expect("write");
    assert!(OrgAccounts::load(dir.path()).is_err());
}

#[test]
fn load_of_a_missing_file_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        OrgAccounts::load(dir.path()).expect("absent file"),
        OrgAccounts::default()
    );
}

/// A github.com origin owned by `owner`.
fn origin(owner: &str) -> String {
    format!("https://github.com/{owner}/jev.git")
}

/// A registry pin for `login`.
fn pin(login: &str) -> RegistryPin {
    RegistryPin {
        account: Some(login.to_string()),
        ..Default::default()
    }
}

/// [`resolve_gh_account_with`] with no SSH aliases.
fn resolve(
    explicit: Option<&str>,
    url: &str,
    pin: impl FnOnce() -> Result<Option<RegistryPin>, String>,
    load: impl FnOnce() -> Result<OrgAccounts, OrgAccountsError>,
) -> Result<Option<ResolvedAccount>, String> {
    resolve_gh_account_with(explicit, url, &SshHostAliases::empty(), pin, load)
}

/// 🔴 #9091: flag beats registry pin beats map beats default.
#[test]
fn flag_beats_pin_beats_map_beats_default() {
    let load = || Ok(map("[accounts]\nduettoresearch = \"bob-duetto\"\n"));
    let duetto = origin("DuettoResearch");
    let got = |r: Result<Option<ResolvedAccount>, String>| {
        r.expect("resolves").map(|r| (r.login, r.source))
    };
    assert_eq!(
        got(resolve(
            Some("bobmatnyc"),
            &duetto,
            || Ok(Some(pin("p"))),
            load
        )),
        Some(("bobmatnyc".into(), AccountSource::Explicit))
    );
    assert_eq!(
        got(resolve(None, &duetto, || Ok(Some(pin("pinned"))), load)),
        Some(("pinned".into(), AccountSource::RegistryPin))
    );
    assert_eq!(
        got(resolve(None, &duetto, || Ok(None), load)),
        Some(("bob-duetto".into(), AccountSource::OrgMap))
    );
    // A pin naming no login still wins: the map is not consulted.
    let no_login = || Ok(Some(RegistryPin::default()));
    assert_eq!(got(resolve(None, &duetto, no_login, load)), None);
    assert_eq!(got(resolve(None, &origin("acme"), || Ok(None), load)), None);
    // A blank flag is no flag: the map applies.
    assert_eq!(
        got(resolve(Some("  "), &duetto, || Ok(None), load)).map(|(_, s)| s),
        Some(AccountSource::OrgMap)
    );
}

/// An explicit selection never reads the registry or the config, so a broken
/// file cannot block a run whose account is already named; neither does an
/// origin that is not on github.com.
#[test]
fn a_flag_never_reads_the_map() {
    let resolved = resolve(
        Some("bob-duetto"),
        &origin("duettoresearch"),
        || panic!("the registry must not be read when a flag is given"),
        || panic!("the map must not be loaded when a flag is given"),
    )
    .expect("ok");
    assert_eq!(resolved.map(|r| r.login).as_deref(), Some("bob-duetto"));
    let local = resolve(
        None,
        "/tmp/some/local/repo",
        || panic!("no owner, no registry read"),
        || panic!("no owner, no map read"),
    );
    assert_eq!(local, Ok(None));
}

/// 🔴 #9091 Fail-Open Check: a load failure reaches the caller, even for an
/// org the broken table might not have named, and names the file.
#[test]
fn a_load_failure_is_returned_not_read_as_unmapped() {
    let err = resolve(
        None,
        &origin("acme"),
        || Ok(None),
        || parse("[accounts]\nduettoresearch = bob-duetto\n"),
    )
    .expect_err("a broken table must refuse");
    assert!(
        err.contains("config.toml") && err.contains("not valid TOML"),
        "{err}"
    );
    let err = resolve(
        None,
        &origin("acme"),
        || Err("registry unreadable".into()),
        || panic!("a pin read failure stops before the map"),
    )
    .expect_err("an unreadable registry must refuse");
    assert!(err.contains("registry unreadable"), "{err}");
}

/// 🔴 #9091: only github.com owners are mapped, after SSH alias resolution.
#[test]
fn owner_of_reads_the_remote_owner() {
    let aliases = SshHostAliases::parse("Host github-bob\n  HostName github.com\n");
    for (url, owner) in [
        (
            "https://github.com/DuettoResearch/jev.git",
            Some("DuettoResearch"),
        ),
        (
            "git@github.com:duettoresearch/jev.git",
            Some("duettoresearch"),
        ),
        ("git@github-bob:duettoresearch/jev", Some("duettoresearch")),
        ("git@github-unknown:duettoresearch/jev", None),
        ("https://gitlab.com/duettoresearch/jev.git", None),
        ("git@gitlab.com:duettoresearch/jev.git", None),
        ("https://bitbucket.org/duettoresearch/jev.git", None),
        ("https://ghe.corp/duettoresearch/jev.git", None),
        ("/tmp/some/local/repo", None),
        ("file:///tmp/some/repo", None),
    ] {
        assert_eq!(owner_of(url, &aliases).as_deref(), owner, "{url}");
    }
}

/// 🔴 #9091: a spawn for an origin no record pins runs as the mapped account.
#[test]
fn an_unpinned_origin_takes_the_mapped_account() {
    let aliases = SshHostAliases::empty();
    let load = || Ok(map("[accounts]\nduettoresearch = \"bob-duetto\"\n"));
    let pinned = org_map_pin("git@github.com:DuettoResearch/jev.git", &aliases, load)
        .expect("not refused")
        .expect("mapped");
    assert_eq!(pinned.account.as_deref(), Some("bob-duetto"));
    assert_eq!(pinned.config_dir, None);
    assert_eq!(pinned.source, AccountSource::OrgMap);
    assert_eq!(
        org_map_pin("https://github.com/acme/widget", &aliases, load).expect("not refused"),
        None
    );
}

/// 🔴 #9091 Fail-Open Check: a broken table never spawns as the active account.
#[test]
fn a_broken_table_fails_the_spawn_closed() {
    let env = org_map_pin(
        "https://github.com/acme/widget",
        &SshHostAliases::empty(),
        || parse("[accounts]\nacme = 7\n"),
    )
    .expect_err("a broken table must fail closed");
    let refused = crate::core::gh_account::REFUSED_GH_TOKEN;
    assert!(
        env.vars
            .iter()
            .any(|(k, v)| k == crate::core::gh_account::GH_TOKEN_ENV_VAR && v == refused),
        "the nobody token must be injected"
    );
    assert!(
        env.warning
            .as_deref()
            .is_some_and(|w| w.contains("[accounts]"))
    );
}

/// #9091: an unproven `[accounts]` login's warning names the table and
/// `gh auth login`, not the registry pin and its `tm projects register` fix.
#[test]
fn an_org_map_refusal_names_the_table_not_the_registry() {
    let mapped = crate::core::gh_account::PinnedGhIdentity {
        account: Some("bob-duetto".into()),
        config_dir: None,
        source: AccountSource::OrgMap,
    };
    let env = crate::core::gh_account::pinned_spawn_env(&mapped, &origin("duetto"), |_| {
        Err("no candidate token".into())
    })
    .expect("a pin")
    .expect("an env");
    let warning = env.warning.expect("a refusal warning");
    assert!(warning.contains("[accounts] table"), "{warning}");
    assert!(warning.contains("gh auth login"), "{warning}");
    assert!(!warning.contains("tm projects register"), "{warning}");
    assert!(!warning.contains("is pinned"), "{warning}");
}
