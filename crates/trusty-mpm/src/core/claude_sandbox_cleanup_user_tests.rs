//! Tests for the #12077 one-time removal of the sandbox keys tm 1.14.14 and
//! 1.14.15 wrote into tm's user tier, `<config_dir>/settings.json`, and for
//! the role-record delete at the end of the same pass.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::*;
use crate::core::session_launch::{SandboxInputs, SandboxRole, apply_launch_sandbox, user_policy};
use crate::secret_source::test_env::EnvVarGuard;

/// The hosts both tags wrote: `sandbox_settings.rs:78-85` at
/// `trusty-mpm-v1.14.14` and at `trusty-mpm-v1.14.15`.
const TAG_DOMAINS: [&str; 6] = [
    "github.com",
    "api.github.com",
    "crates.io",
    "static.crates.io",
    "index.crates.io",
    "registry.npmjs.org",
];

/// The denied variables both tags wrote: `sandbox_settings.rs:90-102` at
/// `trusty-mpm-v1.14.14` and at `trusty-mpm-v1.14.15`.
const TAG_ENV_VARS: [&str; 11] = [
    "ANTHROPIC_API_KEY",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "TELEGRAM_BOT_TOKEN",
    "SLACK_BOT_TOKEN",
    "SLACK_APP_TOKEN",
    "CARGO_REGISTRY_TOKEN",
    "NPM_TOKEN",
];

/// A canonical temp root with a workspace, a home and a tm config dir under
/// it; never the operator's real config dir.
struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    ws: PathBuf,
    home: PathBuf,
    config: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical");
        let (ws, home, config) = (
            root.join("ws"),
            root.join("home"),
            root.join("claude-config"),
        );
        for dir in [&ws, &home, &config] {
            std::fs::create_dir_all(dir).expect("mkdir");
        }
        Self {
            _tmp: tmp,
            root,
            ws,
            home,
            config,
        }
    }

    fn user_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    fn user_marker(&self) -> PathBuf {
        self.config.join(USER_CLEANUP_MARKER_FILE)
    }

    fn project_file(&self) -> PathBuf {
        self.ws.join(".claude/settings.json")
    }

    fn project_marker(&self) -> PathBuf {
        self.ws.join(".trusty-mpm").join(CLEANUP_MARKER_FILE)
    }

    fn record(&self) -> PathBuf {
        self.ws.join(".trusty-mpm").join(ROLE_RECORD_FILE)
    }

    fn clean(&self) -> Result<UserCleanupOutcome, SandboxCleanupError> {
        clean_tm_user_sandbox_keys_once(&self.config, &self.home)
    }

    /// The shared launch step, as every launch path runs it.
    fn launch(&self) {
        let inputs = SandboxInputs::new(&self.ws, &self.home);
        apply_launch_sandbox(SandboxRole::Pm, &inputs, Some(&self.config))
            .expect("the cleanup never refuses a launch");
    }

    /// A 1.14.14 role record and a project file holding its one key.
    fn plant_project(&self, settings: &str) {
        write_text(
            &self.record(),
            &json!({"pm": {"sandbox": {"enabled": true}}}).to_string(),
        );
        write_text(&self.project_file(), settings);
    }
}

const PROJECT: &str = r#"{"sandbox": {"enabled": true}, "model": "opus"}"#;

fn write_text(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

fn write_json(path: &Path, value: &Value) {
    write_text(path, &serde_json::to_string_pretty(value).expect("json"));
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
}

fn sha(path: &Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(path).expect("read")).to_vec()
}

fn tm_env_vars() -> Vec<Value> {
    TAG_ENV_VARS
        .iter()
        .map(|name| json!({"name": name, "mode": "deny"}))
        .collect()
}

/// The operator's own keys in tm's user tier.
fn operator_keys() -> Value {
    json!({
        "model": "opus",
        "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
            {"type": "command", "command": "tm hook --pm-guard"}
        ]}]}
    })
}

/// tm's user tier as 1.14.15 left it: the operator's keys plus tm's values.
fn v1_14_15_user_tier() -> Value {
    let mut value = operator_keys();
    value["sandbox"] = json!({
        "credentials": {"envVars": tm_env_vars()},
        "network": {"allowedDomains": TAG_DOMAINS, "strictAllowlist": true}
    });
    value
}

/// tm's user tier as 1.14.14 left it: no `allowedDomains` beside the flag.
fn v1_14_14_user_tier() -> Value {
    let mut value = v1_14_15_user_tier();
    if let Some(network) = value
        .pointer_mut("/sandbox/network")
        .and_then(Value::as_object_mut)
    {
        network.remove("allowedDomains");
    }
    value
}

/// Make `dir` read-only; `false` on a root runner, which ignores the mode.
#[cfg(unix)]
fn read_only(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    if std::fs::write(dir.join("probe"), b"x").is_ok() {
        eprintln!("skipped: this runner can write a read-only directory");
        return false;
    }
    true
}

#[cfg(unix)]
fn writable(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// #12077: the value set the cleanup matches is exactly what the two tags
/// wrote (`user_policy` at `sandbox_settings.rs:214-227` in 1.14.14 and
/// `:218-232` in 1.14.15; 1.14.15 added `allowedDomains`).
#[test]
fn the_user_tier_value_set_is_what_1_14_14_and_1_14_15_wrote_12077() {
    let ours = user_policy(SandboxRole::Pm).expect("the PM has a user tier");
    assert_eq!(
        ours,
        json!({"sandbox": {
            "network": {"strictAllowlist": true, "allowedDomains": TAG_DOMAINS},
            "credentials": {"envVars": tm_env_vars()}
        }})
    );
}

/// #12077: a user-tier key or entry equal to tm's value goes; the operator's
/// keys stay, and a container left empty goes with tm's entries.
#[test]
fn a_matching_user_tier_key_is_removed_12077() {
    for (tag, planted, count) in [
        ("1.14.15", v1_14_15_user_tier(), 13),
        ("1.14.14", v1_14_14_user_tier(), 12),
    ] {
        let fx = Fx::new();
        write_json(&fx.user_file(), &planted);
        let outcome = fx.clean().expect("cleanup");
        let UserCleanupOutcome::Cleaned { removed, kept } = outcome else {
            panic!("{tag}: expected Cleaned, got {outcome:?}");
        };
        assert_eq!(removed.len(), count, "{tag}: {removed:?}");
        assert!(
            removed
                .iter()
                .any(|k| k.key == "sandbox.network.strictAllowlist"),
            "{tag}: {removed:?}"
        );
        assert!(kept.is_empty(), "{tag}: {kept:?}");
        assert_eq!(read_json(&fx.user_file()), operator_keys(), "{tag}");
        let marker = read_json(&fx.user_marker());
        assert_eq!(marker["tier"], json!("user"), "{marker}");
        assert_eq!(
            marker["removed"].as_array().map(Vec::len),
            Some(count),
            "{marker}"
        );
    }
}

/// #12077: a value other than tm's stays and is reported in the marker; with
/// nothing of tm's the file is not rewritten.
#[test]
fn a_different_user_tier_value_stays_and_is_reported_12077() {
    let fx = Fx::new();
    let text = r#"{"sandbox":{"network":{"strictAllowlist":false,"allowedDomains":["github.com","example.com"]}},"model":"opus"}"#;
    write_text(&fx.user_file(), text);
    let outcome = fx.clean().expect("cleanup");
    let UserCleanupOutcome::Cleaned { removed, mut kept } = outcome else {
        panic!("expected Cleaned, got {outcome:?}");
    };
    assert!(removed.is_empty(), "{removed:?}");
    kept.sort();
    let want = [
        "sandbox.network.allowedDomains",
        "sandbox.network.strictAllowlist",
    ];
    assert_eq!(kept, want);
    assert_eq!(std::fs::read_to_string(fx.user_file()).expect("read"), text);
    let mut reported: Vec<String> = read_json(&fx.user_marker())["kept"]
        .as_array()
        .expect("kept list")
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    reported.sort();
    assert_eq!(reported, want);

    // One key of tm's and one of the operator's: only tm's goes.
    let fx = Fx::new();
    let mut mixed = v1_14_15_user_tier();
    mixed["sandbox"]["network"]["strictAllowlist"] = json!(false);
    write_json(&fx.user_file(), &mixed);
    let outcome = fx.clean().expect("cleanup");
    assert!(
        matches!(&outcome, UserCleanupOutcome::Cleaned { kept, .. }
            if kept == &["sandbox.network.strictAllowlist".to_owned()]),
        "{outcome:?}"
    );
    let after = read_json(&fx.user_file());
    assert_eq!(
        after.pointer("/sandbox/network"),
        Some(&json!({"strictAllowlist": false})),
        "{after}"
    );
    assert!(after.pointer("/sandbox/credentials").is_none(), "{after}");
}

/// #12077: an `envVars` entry the operator added stays, in its place, and is
/// reported; tm's eleven entries go.
#[test]
fn a_user_added_env_vars_entry_stays_12077() {
    let fx = Fx::new();
    let mut planted = v1_14_15_user_tier();
    let mine = json!({"name": "MY_TOKEN", "mode": "deny"});
    if let Some(list) = planted
        .pointer_mut("/sandbox/credentials/envVars")
        .and_then(Value::as_array_mut)
    {
        list.insert(3, mine.clone());
    }
    write_json(&fx.user_file(), &planted);
    let outcome = fx.clean().expect("cleanup");
    let UserCleanupOutcome::Cleaned { removed, kept } = outcome else {
        panic!("expected Cleaned, got {outcome:?}");
    };
    assert_eq!(removed.len(), 13, "{removed:?}");
    assert_eq!(kept, ["sandbox.credentials.envVars[MY_TOKEN]"]);
    let after = read_json(&fx.user_file());
    assert_eq!(
        after.pointer("/sandbox/credentials/envVars"),
        Some(&json!([mine])),
        "{after}"
    );
    assert!(after.pointer("/sandbox/network").is_none(), "{after}");
    assert_eq!(after["model"], json!("opus"));
}

/// #12077 owner answer 4: a file that holds no tm key is never rewritten, in
/// either tier; the hash of each is unchanged after a full launch pass.
#[test]
#[serial_test::serial]
fn a_file_with_no_tm_key_is_byte_identical_in_both_tiers_12077() {
    let _off = EnvVarGuard::unset(SANDBOX_OPT_IN_ENV);
    let fx = Fx::new();
    // Unsorted, compact, and holding only values that are not tm's.
    fx.plant_project(r#"{"zeta":1,"sandbox":{"enabled":false},"alpha":{"b":2,"a":1}}"#);
    write_text(
        &fx.user_file(),
        r#"{"zeta":1,"sandbox":{"network":{"allowedDomains":["example.com"]}},"alpha":{"b":2}}"#,
    );
    let (project, user) = (sha(&fx.project_file()), sha(&fx.user_file()));
    fx.launch();
    assert_eq!(
        sha(&fx.project_file()),
        project,
        "the project file was rewritten"
    );
    assert_eq!(
        sha(&fx.user_file()),
        user,
        "the user-tier file was rewritten"
    );
    assert!(fx.project_marker().exists(), "the project tier ran");
    assert!(fx.user_marker().exists(), "the user tier ran");
}

/// #12077 owner answer 2: the role record is read by the project pass, then
/// deleted when the switch is off; kept when it is on, and kept when the
/// project pass failed, so the retry can still read it.
#[test]
#[serial_test::serial]
fn the_role_record_is_deleted_only_when_the_flag_is_off_12077() {
    {
        let _off = EnvVarGuard::unset(SANDBOX_OPT_IN_ENV);
        let fx = Fx::new();
        fx.plant_project(PROJECT);
        fx.launch();
        let after = read_json(&fx.project_file());
        assert!(
            after.pointer("/sandbox/enabled").is_none(),
            "the record was read before the delete: {after}"
        );
        assert_eq!(after["model"], json!("opus"));
        assert!(!fx.record().exists(), "flag off: the record is deleted");
    }
    {
        let _on = EnvVarGuard::set(SANDBOX_OPT_IN_ENV, "1");
        let fx = Fx::new();
        fx.plant_project(PROJECT);
        fx.launch();
        let after = read_json(&fx.project_file());
        assert!(after.pointer("/sandbox/enabled").is_none(), "{after}");
        assert!(fx.record().exists(), "flag on: the record is kept");
    }
    {
        let _off = EnvVarGuard::unset(SANDBOX_OPT_IN_ENV);
        let fx = Fx::new();
        fx.plant_project("{ not json");
        fx.launch();
        assert!(!fx.project_marker().exists());
        assert!(fx.record().exists(), "a failed pass keeps the record");
    }
}

/// #12077 Fail-Open Check, partial success: the user-tier rewrite fails while
/// the project tier finishes. The core call is an error naming the file; the
/// launch goes ahead; only the project marker is written. The next launch
/// leaves the project file alone and finishes the user tier.
#[cfg(unix)]
#[test]
#[serial_test::serial]
fn a_user_tier_write_failure_marks_only_the_project_tier_then_retries_12077() {
    let _off = EnvVarGuard::unset(SANDBOX_OPT_IN_ENV);
    let fx = Fx::new();
    fx.plant_project(PROJECT);
    write_json(&fx.user_file(), &v1_14_15_user_tier());
    let before = std::fs::read(fx.user_file()).expect("read");
    // The lock is taken, so the failure lands on the temp-file stage.
    write_text(
        &crate::core::settings_lock::lock_sidecar(&fx.user_file()),
        "",
    );
    if !read_only(&fx.config) {
        return;
    }
    let direct = fx.clean();
    fx.launch();
    writable(&fx.config);
    let err = direct.expect_err("a user-tier write failure is an error");
    assert!(matches!(err, SandboxCleanupError::Write { .. }), "{err}");
    assert!(
        err.to_string()
            .contains(&fx.user_file().display().to_string()),
        "{err}"
    );
    assert!(
        !fx.user_marker().exists(),
        "a failure must not finish the user tier"
    );
    assert_eq!(std::fs::read(fx.user_file()).expect("read"), before);
    assert!(fx.project_marker().exists(), "the project tier finished");

    // The operator re-adds a value after the project pass: never removed again.
    write_text(&fx.project_file(), PROJECT);
    fx.launch();
    assert_eq!(
        std::fs::read_to_string(fx.project_file()).expect("read"),
        PROJECT,
        "the finished project tier is not run twice"
    );
    assert_eq!(read_json(&fx.user_file()), operator_keys());
    assert!(
        fx.user_marker().exists(),
        "the retry finished the user tier"
    );
}

/// #12077 Fail-Open Check: a user-tier file that cannot be parsed is an error
/// naming it, not "no file"; no marker, the file untouched.
#[test]
fn a_user_tier_parse_failure_writes_no_marker_12077() {
    let fx = Fx::new();
    write_text(&fx.user_file(), "{ not json");
    let err = fx.clean().expect_err("a parse failure is an error");
    assert!(matches!(err, SandboxCleanupError::Read(_)), "{err}");
    assert!(
        err.to_string()
            .contains(&fx.user_file().display().to_string()),
        "{err}"
    );
    assert!(!fx.user_marker().exists());
    assert_eq!(
        std::fs::read_to_string(fx.user_file()).expect("read"),
        "{ not json"
    );
}

/// #12077: a symlinked user-tier file is skipped: the link stays a link, its
/// target is untouched, and no marker is written.
#[cfg(unix)]
#[test]
fn a_symlinked_user_tier_file_is_skipped_12077() {
    let fx = Fx::new();
    let target = fx.root.join("dotfiles-claude-settings.json");
    write_json(&target, &v1_14_15_user_tier());
    let before = sha(&target);
    std::os::unix::fs::symlink(&target, fx.user_file()).expect("symlink");
    let outcome = fx.clean().expect("cleanup");
    assert_eq!(
        outcome,
        UserCleanupOutcome::SymlinkedSettings(fx.user_file())
    );
    let meta = std::fs::symlink_metadata(fx.user_file()).expect("lstat");
    assert!(meta.file_type().is_symlink(), "the link is kept");
    assert_eq!(sha(&target), before, "the target is untouched");
    assert!(!fx.user_marker().exists(), "a skip is not a finish");
}

/// #12077: a missing user-tier file is not created; the tier is marked done
/// as absent, so a file the operator writes later is never cleaned.
#[test]
fn a_missing_user_tier_file_is_not_created_12077() {
    let fx = Fx::new();
    let outcome = fx.clean().expect("cleanup");
    assert_eq!(outcome, UserCleanupOutcome::NoSettingsFile);
    assert!(!fx.user_file().exists(), "no settings file is created");
    let marker = read_json(&fx.user_marker());
    assert_eq!(marker["tier"], json!("user"), "{marker}");
    assert_eq!(marker["settings"], json!("absent"), "{marker}");
    write_json(&fx.user_file(), &v1_14_15_user_tier());
    let before = sha(&fx.user_file());
    assert_eq!(fx.clean().expect("second"), UserCleanupOutcome::AlreadyDone);
    assert_eq!(sha(&fx.user_file()), before);
}

/// #12077: tm never wrote the sandbox into the operator's `~/.claude`, a
/// relative dir or a dir that does not exist, so none of them is read or
/// written.
#[test]
fn a_dir_tm_never_wrote_is_never_cleaned_12077() {
    let fx = Fx::new();
    let operators = fx.home.join(".claude");
    write_json(&operators.join("settings.json"), &v1_14_15_user_tier());
    let before = sha(&operators.join("settings.json"));
    let outcome = clean_tm_user_sandbox_keys_once(&operators, &fx.home).expect("cleanup");
    assert_eq!(outcome, UserCleanupOutcome::NotTmOwned(operators.clone()));
    assert_eq!(sha(&operators.join("settings.json")), before);
    assert!(!operators.join(USER_CLEANUP_MARKER_FILE).exists());
    let relative = Path::new("claude-config");
    assert_eq!(
        clean_tm_user_sandbox_keys_once(relative, &fx.home).expect("cleanup"),
        UserCleanupOutcome::NotTmOwned(relative.to_path_buf())
    );
    let gone = fx.root.join("no-such-config");
    assert_eq!(
        clean_tm_user_sandbox_keys_once(&gone, &fx.home).expect("cleanup"),
        UserCleanupOutcome::NoConfigDir
    );
    assert!(!gone.exists(), "the config dir is not created");
}
