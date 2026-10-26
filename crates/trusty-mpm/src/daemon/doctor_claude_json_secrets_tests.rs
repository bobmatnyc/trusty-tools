//! Tests for the `claude_json_secrets` row and its `--fix` repair (#10041).
//!
//! Every config dir is a temp dir, every store is a `MemoryKeyStore` or a fake
//! over one, and every value is an obvious fake. No test reads the real
//! managed config dir or the real credential store.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use trusty_common::credentials::{KeyStore, KeyStoreError, MemoryKeyStore};

use super::super::doctor_claude_json_secrets_repair::repair_with_store;
use super::*;
use crate::core::doctor_repair::{RepairMode, StepStatus};

/// An obviously fake Slack bot token.
const FAKE_TOKEN: &str = "xoxb-10041-not-a-real-token";

/// A store whose every write fails.
struct RefusingStore(MemoryKeyStore);

impl KeyStore for RefusingStore {
    fn get(&self, provider: &str) -> Option<String> {
        self.0.get(provider)
    }
    fn set(&self, _provider: &str, _value: &str) -> Result<(), KeyStoreError> {
        Err(KeyStoreError::Io {
            path: PathBuf::from("store.toml"),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        })
    }
    fn unset(&self, provider: &str) -> Result<(), KeyStoreError> {
        self.0.unset(provider)
    }
    fn list(&self) -> Vec<String> {
        self.0.list()
    }
}

/// A managed `.claude.json` with slack-mcp holding the fake token, plus an
/// unrelated top-level key and a non-credential env var.
fn config_with_token() -> Value {
    json!({
        "oauthAccount": { "emailAddress": "someone@example.invalid" },
        "mcpServers": {
            "slack-mcp": {
                "type": "stdio",
                "command": "slack-mcp",
                "args": [],
                "env": { "SLACK_BOT_TOKEN": FAKE_TOKEN, "RUST_LOG": "info" }
            },
            "trusty-search": { "type": "stdio", "command": "trusty-search", "args": ["serve"] }
        }
    })
}

/// Write `value` as `<dir>/.claude.json` at `mode`.
fn write_config(dir: &Path, value: &Value, mode: u32) -> PathBuf {
    let path = dir.join(CLAUDE_JSON);
    std::fs::write(&path, serde_json::to_string_pretty(value).unwrap()).unwrap();
    chmod(&path, mode);
    path
}

fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn clean_config() -> Value {
    json!({ "mcpServers": { "slack-mcp": { "type": "stdio", "command": "slack-mcp", "args": [] } } })
}

#[test]
fn row_fails_on_mode_0644() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &clean_config(), 0o644);

    let row = check_claude_json_secrets(Some(dir.path()));

    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(row.message.contains("0644"), "{}", row.message);
}

#[test]
fn row_passes_on_mode_0600() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &clean_config(), 0o600);

    let row = check_claude_json_secrets(Some(dir.path()));

    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
}

#[test]
fn row_fails_on_a_plaintext_credential() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &config_with_token(), 0o600);

    let row = check_claude_json_secrets(Some(dir.path()));

    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(row.message.contains("slack-mcp.SLACK_BOT_TOKEN"), "{}", row.message);
    assert!(!row.message.contains(FAKE_TOKEN), "value leaked: {}", row.message);
}

#[test]
fn row_is_unknown_on_malformed_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CLAUDE_JSON);
    std::fs::write(&path, "{ not json").unwrap();
    chmod(&path, 0o600);

    let row = check_claude_json_secrets(Some(dir.path()));

    assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
}

#[test]
fn row_fails_on_a_wide_backup() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &clean_config(), 0o600);
    let bak = dir.path().join(".claude.json.bak");
    std::fs::write(&bak, "{}").unwrap();
    chmod(&bak, 0o644);

    let row = check_claude_json_secrets(Some(dir.path()));

    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(row.message.contains(".claude.json.bak"), "{}", row.message);
}

#[test]
fn scan_names_server_and_key_not_value() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &config_with_token(), 0o600);

    let finding = scan_claude_json(dir.path());

    assert_eq!(
        finding.migratable,
        vec![EnvCredential {
            server: "slack-mcp".into(),
            key: "SLACK_BOT_TOKEN".into()
        }]
    );
    assert!(finding.unmapped.is_empty());
    assert!(!format!("{finding:?}").contains(FAKE_TOKEN));
}

/// #10041 closure: no credential value in `.claude.json` after the fix, and
/// the server entry still launches the same command.
#[test]
fn fix_migrates_the_token_and_keeps_the_server_working() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), &config_with_token(), 0o644);
    let store = MemoryKeyStore::new();

    let steps = repair_with_store(dir.path(), RepairMode::Apply, &store);

    assert!(
        steps.iter().all(|s| matches!(s.status, StepStatus::Applied { .. })),
        "{steps:?}"
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains(FAKE_TOKEN), "the token is still in the file");
    let after: Value = serde_json::from_str(&text).unwrap();
    let slack = &after["mcpServers"]["slack-mcp"];
    assert_eq!(slack["command"], "slack-mcp");
    assert_eq!(slack["env"], json!({ "RUST_LOG": "info" }));
    assert_eq!(after["mcpServers"]["trusty-search"]["args"], json!(["serve"]));
    assert_eq!(after["oauthAccount"]["emailAddress"], "someone@example.invalid");
    assert_eq!(store.get("slack").as_deref(), Some(FAKE_TOKEN));
    assert_eq!(mode(&path), 0o600);
    assert!(!format!("{steps:?}").contains(FAKE_TOKEN));
    assert_eq!(
        check_claude_json_secrets(Some(dir.path())).status,
        CheckStatus::Ok
    );
}

/// #10041 failure path: a store miss never strips the value, so the server
/// keeps its working token, and the file bytes do not change.
#[test]
fn fix_leaves_the_file_untouched_when_the_store_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), &config_with_token(), 0o600);
    let before = std::fs::read(&path).unwrap();
    let store = RefusingStore(MemoryKeyStore::new());

    let steps = repair_with_store(dir.path(), RepairMode::Apply, &store);

    assert!(
        steps.iter().any(|s| matches!(s.status, StepStatus::Failed(_))),
        "{steps:?}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!format!("{steps:?}").contains(FAKE_TOKEN));
}

/// #10041 failure path: a rewrite that cannot be published leaves the
/// original bytes, never a half-written `.claude.json`.
#[test]
fn fix_leaves_the_original_bytes_when_the_rewrite_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), &config_with_token(), 0o600);
    let before = std::fs::read(&path).unwrap();
    let store = MemoryKeyStore::new();
    // A read-only directory: the staging file beside the target cannot be made.
    chmod(dir.path(), 0o500);

    let steps = repair_with_store(dir.path(), RepairMode::Apply, &store);

    chmod(dir.path(), 0o700);
    let failed = steps
        .iter()
        .find_map(|s| match &s.status {
            StepStatus::Failed(why) => Some(why.clone()),
            _ => None,
        })
        .expect("the rewrite must fail");
    assert!(failed.contains("original is unchanged"), "{failed}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .filter(|n| n != CLAUDE_JSON)
        .collect();
    assert!(leftovers.is_empty(), "staging litter: {leftovers:?}");
}

#[test]
fn fix_dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), &config_with_token(), 0o644);
    let before = std::fs::read(&path).unwrap();
    let store = MemoryKeyStore::new();

    let steps = repair_with_store(dir.path(), RepairMode::DryRun, &store);

    assert!(steps.iter().all(|s| s.status == StepStatus::Planned), "{steps:?}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(mode(&path), 0o644);
    assert!(store.list().is_empty());
}

#[test]
fn fix_tightens_a_wide_file_and_its_backups() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), &clean_config(), 0o644);
    let bak = dir.path().join(".claude.json.bak");
    std::fs::write(&bak, "{}").unwrap();
    chmod(&bak, 0o644);

    let steps = repair_with_store(dir.path(), RepairMode::Apply, &MemoryKeyStore::new());

    assert_eq!(steps.len(), 2, "{steps:?}");
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&bak), 0o600);
}

#[test]
fn fix_is_silent_on_a_clean_dir() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &clean_config(), 0o600);

    let steps = repair_with_store(dir.path(), RepairMode::Apply, &MemoryKeyStore::new());

    assert!(steps.is_empty(), "{steps:?}");
}
