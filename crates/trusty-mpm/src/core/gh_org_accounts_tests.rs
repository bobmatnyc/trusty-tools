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
/// TOML syntax error. It must surface, not read as "no mapping".
#[test]
fn unparseable_toml_is_an_error_not_an_empty_map() {
    let err = parse("[accounts]\nduettoresearch = bob-duetto\n").expect_err("syntax error");
    assert!(matches!(err, OrgAccountsError::Parse { .. }), "{err:?}");
    assert!(err.to_string().contains("config.toml"), "{err}");
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

/// 🔴 #9091: flag beats map beats default.
#[test]
fn flag_beats_map_beats_default() {
    let load = || Ok(map("[accounts]\nduettoresearch = \"bob-duetto\"\n"));
    let flagged = resolve_gh_account(Some("bobmatnyc"), Some("duettoresearch"), load)
        .expect("resolves")
        .expect("some");
    assert_eq!(
        (flagged.login.as_str(), flagged.source),
        ("bobmatnyc", AccountSource::Explicit)
    );

    let mapped = resolve_gh_account(None, Some("DuettoResearch"), load)
        .expect("resolves")
        .expect("some");
    assert_eq!(
        (mapped.login.as_str(), mapped.source),
        ("bob-duetto", AccountSource::OrgMap)
    );

    assert_eq!(
        resolve_gh_account(None, Some("acme"), load).expect("ok"),
        None
    );
    assert_eq!(resolve_gh_account(None, None, load).expect("ok"), None);
    // A blank flag is no flag: the map applies.
    let blank = resolve_gh_account(Some("  "), Some("duettoresearch"), load)
        .expect("ok")
        .expect("some");
    assert_eq!(blank.source, AccountSource::OrgMap);
}

/// An explicit selection never reads the config, so a broken file cannot block
/// a run whose account is already named.
#[test]
fn a_flag_never_reads_the_map() {
    let resolved = resolve_gh_account(Some("bob-duetto"), Some("duettoresearch"), || {
        panic!("the map must not be loaded when a flag is given")
    })
    .expect("ok");
    assert_eq!(resolved.map(|r| r.login).as_deref(), Some("bob-duetto"));
}

/// 🔴 #9091 Fail-Open Check: a load failure reaches the caller, even for an
/// org the broken table might not have named.
#[test]
fn a_load_failure_is_returned_not_read_as_unmapped() {
    let err = resolve_gh_account(None, Some("acme"), || {
        parse("[accounts]\nduettoresearch = bob-duetto\n")
    })
    .expect_err("a broken table must refuse");
    assert!(matches!(err, OrgAccountsError::Parse { .. }), "{err:?}");
}

#[test]
fn owner_of_reads_the_remote_owner() {
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
        ("/tmp/some/local/repo", None),
        ("file:///tmp/some/repo", None),
    ] {
        assert_eq!(owner_of(url).as_deref(), owner, "{url}");
    }
}

/// 🔴 #9091: a spawn for an origin no record pins runs as the mapped account.
#[test]
fn an_unpinned_origin_takes_the_mapped_account() {
    let load = || Ok(map("[accounts]\nduettoresearch = \"bob-duetto\"\n"));
    let pinned = org_map_pin("git@github.com:DuettoResearch/jev.git", load)
        .expect("not refused")
        .expect("mapped");
    assert_eq!(pinned.account.as_deref(), Some("bob-duetto"));
    assert_eq!(pinned.config_dir, None);
    assert_eq!(
        org_map_pin("https://github.com/acme/widget", load).expect("not refused"),
        None
    );
}

/// 🔴 #9091 Fail-Open Check: a broken table never spawns as the active account.
#[test]
fn a_broken_table_fails_the_spawn_closed() {
    let env = org_map_pin("https://github.com/acme/widget", || {
        parse("[accounts]\nacme = 7\n")
    })
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
