//! Tests for `[accounts]` parsing and the rendered block (#11390).

use std::path::Path;

use crate::core::project_accounts::{ACCOUNT_RULE, account_block, accounts_for};
use crate::core::project_config::{ProjectLevelConfig, load_or_report};

fn parse(raw: &str) -> Result<ProjectLevelConfig, String> {
    ProjectLevelConfig::from_toml(raw, Path::new(".trusty-mpm.toml")).map_err(|e| format!("{e:#}"))
}

#[test]
fn accounts_table_parses_names_scope_and_verify() {
    let cfg = parse(
        "[accounts.neon]\naccount = \"robert.matsuoka\"\nscope = \"Duetto-CTO\"\nverify = \"neonctl me\"\n",
    )
    .expect("parses");
    let accounts = cfg.accounts.expect("accounts");
    let entry = &accounts.entries()["neon"];
    assert_eq!(entry.account, "robert.matsuoka");
    assert_eq!(entry.scope.as_deref(), Some("Duetto-CTO"));
    assert_eq!(entry.verify.as_deref(), Some("neonctl me"));
}

/// Remove the detector call and this parses, failing the test.
#[test]
fn secret_shaped_account_value_rejects_the_file() {
    let token = format!("ghp_{}", "Z".repeat(36));
    for raw in [
        format!("[accounts.gh]\naccount = \"{token}\"\n"),
        format!(
            "[accounts.gh]\naccount = \"me\"\nverify = \"gh auth login --with-token {token}\"\n"
        ),
        "[accounts.aws]\naccount = \"me\"\nscope = \"password=hunter2hunter2\"\n".to_string(),
    ] {
        let err = parse(&raw).expect_err("a credential-shaped value must reject the file");
        assert!(err.contains("looks like a credential"), "{err}");
        assert!(
            !err.contains(&token),
            "the error must not echo the value: {err}"
        );
    }
}

#[test]
fn unknown_account_key_rejects_the_file() {
    let err =
        parse("[accounts.gh]\naccount = \"me\"\npassword = \"x\"\n").expect_err("unknown key");
    assert!(err.contains("password"), "{err}");
    parse("[accounts.gh]\nscope = \"x\"\n").expect_err("account is required");
}

#[test]
fn multiline_value_is_rejected() {
    parse("[accounts.gh]\naccount = \"me\\n## injected\"\n").expect_err("newline");
    parse("[accounts.gh]\naccount = \"\"\n").expect_err("empty");
}

#[test]
fn block_lists_each_service_and_the_rule() {
    let cfg = parse("[accounts.gh]\naccount = \"bobmatnyc\"\n").expect("parses");
    let block = account_block(cfg.accounts.as_ref());
    assert!(block.contains("- `gh`: account `bobmatnyc`; verify: the CLI's own whoami command"));
    assert!(block.ends_with(ACCOUNT_RULE));
}

#[test]
fn block_without_a_map_says_so() {
    let block = account_block(None);
    assert_eq!(block.matches("No account map is configured").count(), 1);
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
    assert!(accounts_for(&sub).is_some());
    std::fs::write(
        dir.path().join(".trusty-mpm.toml"),
        "[accounts.gh]\nbogus = 1\n",
    )
    .expect("write");
    assert!(load_or_report(dir.path()).is_none());
    assert!(accounts_for(&sub).is_none());
}

/// The committed config of this repository parses and carries the five services.
#[test]
fn this_repository_config_names_its_accounts() {
    let root = crate::core::content_source::test_support::repo_root();
    let cfg = ProjectLevelConfig::load(&root)
        .expect("parses")
        .expect("present");
    let accounts = cfg.accounts.expect("accounts table");
    let names: Vec<&str> = accounts.entries().keys().map(String::as_str).collect();
    assert_eq!(names, ["aws", "crates-io", "gh", "neon", "vercel"]);
}
