//! Tests for the opt-in `gcloud_auth` doctor row (#8371).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::*;

/// The token every scripted mint returns. No row may ever contain it.
const TOKEN: &str = "ya29.a0AfB_SECRET-TOKEN-VALUE-0123456789abcdef";

/// The reauth refusal real `gcloud` 569 prints (captured 2026-10-02).
const REAUTH_STDERR: &str = "ERROR: (gcloud.auth.print-access-token) There was a problem \
    refreshing your current auth tokens: Reauthentication failed. cannot prompt during \
    non-interactive execution.\nPlease run:\n\n  $ gcloud auth login\n\nto obtain new credentials.";

type Calls = Arc<Mutex<Vec<Vec<String>>>>;

/// A runner that answers from a script and records every call it receives.
struct Scripted {
    calls: Calls,
    list: Run,
    mint: HashMap<String, Run>,
}

impl Scripted {
    fn new(list: Run) -> Self {
        Self {
            calls: Arc::default(),
            list,
            mint: HashMap::new(),
        }
    }

    fn mint(mut self, account: &str, run: Run) -> Self {
        self.mint.insert(account.to_string(), run);
        self
    }
}

impl GcloudRunner for Scripted {
    fn run(&mut self, args: &[String]) -> Run {
        self.calls.lock().expect("calls lock").push(args.to_vec());
        if args.get(1).map(String::as_str) == Some("list") {
            return self.list.clone();
        }
        let account = args
            .iter()
            .find_map(|a| a.strip_prefix("--account="))
            .expect("a mint names its account");
        self.mint
            .get(account)
            .cloned()
            .unwrap_or_else(|| Run::Error(format!("unscripted account {account}")))
    }
}

fn ok(stdout: &str) -> Run {
    Run::Exited {
        success: true,
        code: Some(0),
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

fn err(code: i32, stderr: &str) -> Run {
    Run::Exited {
        success: false,
        code: Some(code),
        stdout: String::new(),
        stderr: stderr.to_string(),
    }
}

fn accounts(entries: &[(&str, bool)]) -> Run {
    let list: Vec<serde_json::Value> = entries
        .iter()
        .map(|(account, active)| {
            serde_json::json!({
                "account": account,
                "status": if *active { "ACTIVE" } else { "" },
            })
        })
        .collect();
    ok(&serde_json::Value::Array(list).to_string())
}

/// Assert no secret and no account local part leaked into `check`.
fn assert_redacted(check: &DoctorCheck, local_parts: &[&str]) {
    assert!(
        !check.message.contains(TOKEN),
        "token leaked: {}",
        check.message
    );
    assert!(
        !check.message.contains("ya29."),
        "token prefix leaked: {}",
        check.message
    );
    for local in local_parts {
        assert!(
            !check.message.contains(&format!("{local}@")),
            "account {local} leaked: {}",
            check.message
        );
    }
}

#[tokio::test]
async fn the_live_wrapper_never_runs_gcloud_without_network() {
    let runner = Scripted::new(accounts(&[("alice@example.com", true)]))
        .mint("alice@example.com", ok(TOKEN));
    let calls = Arc::clone(&runner.calls);
    let check = row_on_blocking_pool(false, runner).await;
    assert!(calls.lock().expect("calls lock").is_empty(), "gcloud ran");
    assert_eq!(check.name, CHECK);
    assert!(
        check.message.starts_with("skipped (needs --network)"),
        "{}",
        check.message
    );
}

#[tokio::test]
async fn the_live_wrapper_runs_gcloud_with_network() {
    let runner = Scripted::new(accounts(&[("alice@example.com", true)]))
        .mint("alice@example.com", ok(TOKEN));
    let calls = Arc::clone(&runner.calls);
    let check = row_on_blocking_pool(true, runner).await;
    assert_eq!(calls.lock().expect("calls lock").len(), 2);
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
}

#[test]
fn an_active_account_that_mints_a_token_is_ok_and_the_token_never_prints() {
    let mut runner = Scripted::new(accounts(&[
        ("bob@other.org", false),
        ("alice@example.com", true),
    ]))
    .mint("alice@example.com", ok(&format!("{TOKEN}\n")));
    let check = gcloud_auth_check(&mut runner);
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("a***@example.com"),
        "{}",
        check.message
    );
    assert!(
        check.message.contains("2 credentialed"),
        "{}",
        check.message
    );
    assert_redacted(&check, &["alice", "bob"]);
}

#[test]
fn no_credentialed_account_fails_naming_the_login_step() {
    let mut runner = Scripted::new(ok("[]"));
    let check = gcloud_auth_check(&mut runner);
    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(
        check.message.starts_with("NO CREDENTIALS"),
        "{}",
        check.message
    );
    assert!(
        check.message.contains("`! gcloud auth login`"),
        "{}",
        check.message
    );
}

#[test]
fn every_account_needing_reauth_fails_distinct_from_no_credentials() {
    let mut runner = Scripted::new(accounts(&[
        ("alice@example.com", true),
        ("bob@other.org", false),
    ]))
    .mint("alice@example.com", err(1, REAUTH_STDERR))
    .mint("bob@other.org", err(1, REAUTH_STDERR));
    let calls = Arc::clone(&runner.calls);
    let check = gcloud_auth_check(&mut runner);
    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(
        check.message.starts_with("REAUTH NEEDED"),
        "{}",
        check.message
    );
    assert!(
        check.message.contains("`! gcloud auth login`"),
        "{}",
        check.message
    );
    assert_eq!(
        calls.lock().expect("calls lock").len(),
        3,
        "both accounts probed"
    );
    assert_redacted(&check, &["alice", "bob"]);
}

#[test]
fn reauth_probing_stops_at_max_probed_accounts() {
    let names: Vec<String> = (0..5).map(|i| format!("user{i}@example.com")).collect();
    let entries: Vec<(&str, bool)> = names.iter().map(|n| (n.as_str(), false)).collect();
    let mut runner = Scripted::new(accounts(&entries));
    for name in &names {
        runner = runner.mint(name, err(1, REAUTH_STDERR));
    }
    let calls = Arc::clone(&runner.calls);
    let check = gcloud_auth_check(&mut runner);
    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert_eq!(calls.lock().expect("calls lock").len(), 1 + MAX_PROBED);
}

#[test]
fn an_active_account_needing_reauth_beside_a_working_one_warns() {
    let mut runner = Scripted::new(accounts(&[
        ("alice@example.com", true),
        ("bob@other.org", false),
    ]))
    .mint("alice@example.com", err(1, REAUTH_STDERR))
    .mint("bob@other.org", ok(TOKEN));
    let check = gcloud_auth_check(&mut runner);
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(
        check.message.contains("b***@other.org can mint"),
        "{}",
        check.message
    );
    assert_redacted(&check, &["alice", "bob"]);
}

/// Fail-Open Check: every way a probe can fail is a non-`Ok` row naming why.
#[test]
fn every_probe_failure_is_a_warn_naming_the_reason_never_ok() {
    let active = || accounts(&[("alice@example.com", true)]);
    let leaky = format!("ERROR: quota exceeded for alice@example.com token {TOKEN}");
    let cases: Vec<(&str, Scripted, &str)> = vec![
        (
            "missing binary",
            Scripted::new(Run::NotFound),
            "not on PATH",
        ),
        (
            "list timeout",
            Scripted::new(Run::TimedOut),
            "did not answer within 10s",
        ),
        (
            "list non-zero",
            Scripted::new(err(2, "ERROR: boom")),
            "exited 2: ERROR: boom",
        ),
        (
            "list unparseable",
            Scripted::new(ok("not json")),
            "not an account list",
        ),
        (
            "mint timeout",
            Scripted::new(active()).mint("alice@example.com", Run::TimedOut),
            "did not answer",
        ),
        (
            "mint non-zero",
            Scripted::new(active()).mint("alice@example.com", err(1, &leaky)),
            "exited 1: ERROR: quota exceeded",
        ),
        (
            "mint empty",
            Scripted::new(active()).mint("alice@example.com", ok("  \n")),
            "printed no token",
        ),
        (
            "mint killed",
            Scripted::new(active()).mint(
                "alice@example.com",
                Run::Exited {
                    success: false,
                    code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                },
            ),
            "killed by a signal",
        ),
    ];
    for (label, mut runner, reason) in cases {
        let check = gcloud_auth_check(&mut runner);
        assert_eq!(
            check.status,
            CheckStatus::Warn,
            "{label}: {}",
            check.message
        );
        assert!(check.message.contains(reason), "{label}: {}", check.message);
        assert_redacted(&check, &["alice"]);
    }
}

#[test]
fn redact_text_strips_tokens_and_email_local_parts() {
    let line = format!("token {TOKEN} refresh 1//0gAbCdEf for carol.x+y@corp.example.com");
    let redacted = redact_text(&line);
    assert!(!redacted.contains("ya29."), "{redacted}");
    assert!(!redacted.contains("1//0g"), "{redacted}");
    assert!(!redacted.contains("carol"), "{redacted}");
    assert!(redacted.contains("***@corp.example.com"), "{redacted}");
    assert_eq!(redact_account("dave@x.io"), "d***@x.io");
    assert_eq!(redact_account("no-at-sign"), "***");
}
