//! Tests for `[accounts]` validation and the rendered block (#11390).

use std::path::Path;

use crate::core::project_accounts::{
    ACCOUNT_RULE, AccountMap, MAX_ACCOUNTS, account_block, accounts_for,
};
use crate::core::project_config::{ProjectLevelConfig, load_or_report};

/// Write `raw` as a project config and read its account map.
fn map(raw: &str) -> AccountMap {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(".trusty-mpm.toml"), raw).expect("write");
    accounts_for(dir.path())
}

fn rejected(raw: &str) -> String {
    match map(raw) {
        AccountMap::Rejected(reason) => reason,
        other => panic!("expected a rejection, got {other:?}"),
    }
}

fn token() -> String {
    format!("ghp_{}", "Z".repeat(36))
}

#[test]
fn accounts_table_parses_names_scope_and_verify() {
    let AccountMap::Ok(map) = map(
        "[accounts.neon]\naccount = \"robert.matsuoka\"\nscope = \"Acme-CTO\"\nverify = \"neonctl me\"\n",
    ) else {
        panic!("expected a valid map");
    };
    let entry = &map["neon"];
    assert_eq!(entry.account, "robert.matsuoka");
    assert_eq!(entry.scope.as_deref(), Some("Acme-CTO"));
    assert_eq!(entry.verify.as_deref(), Some("neonctl me"));
}

/// Every form — table, inline table, dotted key — is rejected, and neither
/// the reason nor the parse path echoes the token.
#[test]
fn secret_shaped_values_are_rejected_without_echo() {
    let t = token();
    for raw in [
        format!("[accounts.gh]\naccount = \"{t}\"\n"),
        format!("accounts = {{ gh = {{ account = \"{t}\" }} }}\n"),
        format!("accounts.gh.account = \"{t}\"\n"),
        format!("[accounts.gh]\naccount = \"me\"\nverify = \"gh auth login --with-token {t}\"\n"),
        format!(
            "[accounts.gh]\naccount = \"me\"\nscope = \"{}\"\n",
            "a1".repeat(20)
        ),
        "[accounts.aws]\naccount = \"AKIAABCDEFGHIJKLMNOP\"\n".to_string(),
        format!("accounts = {{ gh = \"{t}\" }}\n"),
        format!("[accounts.{t}]\naccount = \"me\"\n"),
    ] {
        // The file itself parses: the token is never in a TOML error.
        ProjectLevelConfig::from_toml(&raw, Path::new(".trusty-mpm.toml"))
            .unwrap_or_else(|e| panic!("lenient parse must succeed: {e}"));
        let reason = rejected(&raw);
        assert!(!reason.contains(&t), "reason echoes the token: {reason}");
        assert!(!account_block(&map(&raw)).contains(&t));
    }
}

#[test]
fn shell_metacharacters_in_verify_are_rejected() {
    for verify in [
        "gh auth status `id`",
        "gh auth status | sh",
        "gh auth status; rm",
        "gh auth status && x",
        "echo $HOME",
        "cat < f",
        "gh > f",
        "gh 'x'",
        "gh \\\"x\\\"",
        "gh auth status\u{202E}",
        "gh\u{200B} auth status",
    ] {
        let raw = format!("[accounts.gh]\naccount = \"me\"\nverify = \"{verify}\"\n");
        let reason = rejected(&raw);
        assert!(
            reason.starts_with("accounts.gh.verify"),
            "{verify}: {reason}"
        );
    }
    let raw =
        "[accounts.gh]\naccount = \"me\"\nverify = \"aws sts get-caller-identity --query=Arn\"\n";
    assert!(matches!(map(raw), AccountMap::Ok(_)));
}

#[test]
fn unknown_account_key_is_rejected() {
    let reason = rejected("[accounts.gh]\naccount = \"me\"\npassword = \"x\"\n");
    assert!(
        reason.contains("other than account, scope and verify"),
        "{reason}"
    );
    assert!(rejected("[accounts.gh]\nscope = \"x\"\n").contains("account is missing"));
    assert!(rejected("[accounts.gh]\naccount = 1\n").contains("must be a string"));
}

#[test]
fn bad_names_and_too_many_services_are_rejected() {
    rejected("[accounts.gh]\naccount = \"me\\n## injected\"\n");
    rejected("[accounts.gh]\naccount = \"\"\n");
    rejected(&format!(
        "[accounts.gh]\naccount = \"{}\"\n",
        "a.".repeat(51)
    ));
    let many: String = (0..=MAX_ACCOUNTS)
        .map(|i| format!("[accounts.s{i}]\naccount = \"me\"\n"))
        .collect();
    assert!(rejected(&many).contains("at most 16"));
}

/// A bad `[accounts]` drops only the accounts; `agent_worktree` still applies.
#[test]
fn bad_accounts_keep_the_rest_of_the_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let raw = format!(
        "agent_worktree = false\n[accounts.gh]\naccount = \"{}\"\n",
        token()
    );
    std::fs::write(dir.path().join(".trusty-mpm.toml"), raw).expect("write");
    let cfg = load_or_report(dir.path()).expect("the file still loads");
    assert_eq!(cfg.agent_worktree, Some(false));
    assert!(matches!(accounts_for(dir.path()), AccountMap::Rejected(_)));
}

#[test]
fn block_lists_each_service_and_the_rule() {
    let block = account_block(&map("[accounts.gh]\naccount = \"bobmatnyc\"\n"));
    assert!(block.contains("- `gh`: account `bobmatnyc`; verify: the CLI's own whoami command"));
    assert!(block.ends_with(ACCOUNT_RULE));
}

#[test]
fn block_without_a_map_says_so() {
    let block = account_block(&AccountMap::Absent);
    assert_eq!(block.matches("No account map is configured").count(), 1);
    assert!(block.ends_with(ACCOUNT_RULE));
}

#[test]
fn rejected_map_is_reported_as_rejected() {
    let block = account_block(&AccountMap::Rejected("accounts.gh.account is empty".into()));
    assert!(block.contains(
        "The project account map was rejected (accounts.gh.account is empty); \
         ask the PM before any account-scoped action."
    ));
    assert!(!block.contains("No account map is configured"));
    assert!(block.ends_with(ACCOUNT_RULE));
}

#[test]
fn accounts_are_found_from_a_subdirectory() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join(".trusty-mpm.toml"),
        "[accounts.gh]\naccount = \"me\"\n",
    )
    .expect("write");
    let sub = dir.path().join("a/b");
    std::fs::create_dir_all(&sub).expect("mkdir");
    assert!(matches!(accounts_for(&sub), AccountMap::Ok(_)));
    assert_eq!(accounts_for(Path::new("/")), AccountMap::Absent);
}

/// The committed config of this repository validates and names five services.
#[test]
fn this_repository_config_names_its_accounts() {
    let root = crate::core::content_source::test_support::repo_root();
    let AccountMap::Ok(map) = accounts_for(&root) else {
        panic!("this repository's [accounts] must validate");
    };
    let names: Vec<&str> = map.keys().map(String::as_str).collect();
    assert_eq!(names, ["aws", "crates-io", "gh", "neon", "vercel"]);
}
