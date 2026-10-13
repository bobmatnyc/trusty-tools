//! Tests for [`super::GitHubActionsBackend`] over [`GhShim`], a fake `gh`
//! run by absolute path (#9069). No test runs the real `gh`, reads the real
//! machine config, or changes the process environment.
//!
//! Test: itself.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::settings::steering_vars;
use super::shim::{GhShim, identity};
use super::*;
use crate::store::cli::test_shim::install_script;

/// A value with no trailing newline, a quote, a space and non-ASCII text.
const VALUE: &str = "ghs-canary-9069 \"q\" é-0123456789abcdef";

fn vault(name: &str) -> VaultName {
    VaultName::new(name).unwrap()
}

fn key(name: &str) -> SecretKey {
    SecretKey::new(name).unwrap()
}

fn web() -> VaultName {
    vault("trusty/acme/web")
}

/// Push `name` = [`VALUE`] through `backend`.
fn push(backend: &GitHubActionsBackend, name: &str) -> Result<(), SecretsError> {
    backend.set(&web(), &key(name), &SecretValue::new(VALUE))
}

/// The `Backend` error's reason, or a panic for any other result.
fn reason(result: Result<(), SecretsError>) -> String {
    match result {
        Err(SecretsError::Backend {
            backend, reason, ..
        }) => {
            assert_eq!(backend, BackendId::GITHUB_ACTIONS);
            reason
        }
        other => panic!("expected a backend error, got {other:?}"),
    }
}

/// Why: C2, G1 — the value reaches `gh` on stdin only, byte-exact with no
/// newline added, and the push names the origin repository and the Actions
/// app; the baseline and the confirming read bracket it. A new secret is
/// confirmed by its `updated_at` appearing.
/// Test: itself.
#[test]
fn github_push_writes_the_value_on_stdin_only() {
    let shim = GhShim::new();
    push(&shim.backend(), "API_KEY").unwrap();

    assert_eq!(shim.stdin("API_KEY").as_deref(), Some(VALUE));
    let calls = shim.calls();
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(
        lines,
        [
            "api --method GET repos/acme/web/actions/secrets/API_KEY",
            "secret set API_KEY --repo github.com/acme/web --app actions",
            "api --method GET repos/acme/web/actions/secrets/API_KEY",
        ]
    );
    assert!(!calls.contains(VALUE) && !shim.env_log().contains(VALUE));
    assert!(!format!("{:?}", shim.backend()).contains(VALUE));
}

/// Why: G1 — the vault a key is read from never picks the repository; a
/// push from an overridden vault still goes to the backend's origin
/// identity.
/// Test: itself.
#[test]
fn github_vault_name_never_picks_the_repository() {
    let shim = GhShim::new();
    let shared = vault("trusty/other/shared");
    shim.backend()
        .set(&shared, &key("API_KEY"), &SecretValue::new(VALUE))
        .unwrap();
    let calls = shim.calls();
    assert!(calls.contains("--repo github.com/acme/web "), "{calls}");
    assert!(
        !calls.contains("other") && !calls.contains("shared"),
        "{calls}"
    );
}

/// Why: G2, D7 — every inherited `GH_*` name and both token names are
/// removed, whatever else is inherited, and `GH_CONFIG_DIR` reaches `gh`
/// only from the machine config.
/// Test: itself.
#[test]
fn github_child_env_carries_no_inherited_token_or_repo() {
    let inherited = [
        "HOME",
        "GH_PROMPT_DISABLED",
        "PATH",
        "GH_DEBUG",
        "GITHUB_ACTIONS",
    ]
    .map(OsString::from);
    let removed = steering_vars(inherited);
    for name in [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
        "GH_HOST",
        "GH_REPO",
        "GH_CONFIG_DIR",
        "GH_PROMPT_DISABLED",
        "GH_DEBUG",
    ] {
        assert!(removed.contains(&OsString::from(name)), "{name}");
    }
    for kept in ["HOME", "PATH", "GITHUB_ACTIONS"] {
        assert!(!removed.contains(&OsString::from(kept)), "{kept}");
    }

    let shim = GhShim::new();
    let config_dir = shim.dir().join("ghconfig");
    std::fs::create_dir(&config_dir).unwrap();
    let mut settings = shim.settings();
    settings.config_dir = Some(config_dir.clone());
    push(&shim.backend_with(settings), "API_KEY").unwrap();
    let env = shim.env_log();
    let configured = format!("GH_CONFIG_DIR={}", config_dir.display());
    for line in env.lines() {
        let steering = line.starts_with("GH_") || line.starts_with("GITHUB_TOKEN=");
        assert!(!steering || line == configured, "{line}");
    }
    assert_eq!(env.matches(&configured).count(), 3, "{env}");
}

/// Why: C3, G3, D8 — exit 0 alone never counts. A push that stores nothing
/// leaves a new secret absent and an existing one's `updated_at` where it
/// was; both are failures whose text says the value may not have landed.
/// Test: itself.
#[test]
fn github_exit_zero_without_a_newer_updated_at_is_a_failure() {
    let shim = GhShim::new();
    shim.noop();
    let absent = reason(push(&shim.backend(), "NEW_KEY"));
    assert!(absent.contains("may not have landed"), "{absent}");

    shim.holds("OLD_KEY", "2026-10-12T00:00:05Z");
    let stale = reason(push(&shim.backend(), "OLD_KEY"));
    assert!(stale.contains("may not have landed"), "{stale}");
    assert_eq!(
        shim.stdin("OLD_KEY").as_deref(),
        Some(VALUE),
        "it was pushed"
    );

    // The positive control: an update that advances `updated_at` counts.
    let shim = GhShim::new();
    shim.holds("OLD_KEY", "2026-10-12T00:00:05Z");
    push(&shim.backend(), "OLD_KEY").unwrap();
    assert_eq!(
        shim.updated_at("OLD_KEY").as_deref(),
        Some("2026-10-12T00:00:11Z")
    );
}

/// Why: C4 — a push `gh` rejects is a backend error, and its stderr, which
/// echoed the value, never reaches the error.
/// Test: itself.
#[test]
fn github_non_zero_push_is_an_error_that_withholds_output() {
    let shim = GhShim::new();
    shim.fail_push("API_KEY");
    let failed = push(&shim.backend(), "API_KEY").unwrap_err();
    let text = format!("{failed} {failed:?}");
    assert!(matches!(failed, SecretsError::Backend { .. }), "{text}");
    assert!(
        !text.contains(VALUE) && !text.contains("HTTP 422"),
        "{text}"
    );
    assert_eq!(
        shim.calls().lines().count(),
        2,
        "no confirming read after a failed push"
    );
}

/// Why: G4 — a signed-out `gh` is `BackendLocked` with the fixed login
/// hint, found by the baseline read, and the value is never pushed.
/// Test: itself.
#[test]
fn github_signed_out_preflight_is_backend_locked_and_never_pushes() {
    let shim = GhShim::new();
    shim.locked();
    match push(&shim.backend(), "API_KEY") {
        Err(SecretsError::BackendLocked { backend, hint }) => {
            assert_eq!(backend, BackendId::GITHUB_ACTIONS);
            assert!(hint.contains("gh auth login"), "{hint}");
        }
        other => panic!("expected BackendLocked, got {other:?}"),
    }
    assert_eq!(shim.stdin("API_KEY"), None, "nothing was pushed");
    assert_eq!(shim.calls().lines().count(), 1);
}

/// Why: C4 — a push that hangs is killed at its timeout and fails the key.
/// Test: itself.
#[test]
fn github_timeout_kills_the_group_and_fails_the_key() {
    let shim = GhShim::new();
    shim.slow();
    let mut settings = shim.settings();
    settings.timeout = Duration::from_millis(300);
    let started = Instant::now();
    let failed = reason(push(&shim.backend_with(settings), "API_KEY"));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert!(failed.contains("timeout"), "{failed}");
}

/// Why: C4, D8 — a push followed by a confirming read that fails is a
/// failure, fail closed, and its text says the value may have landed.
/// Test: itself.
#[test]
fn github_verify_read_failure_after_push_fails_the_key() {
    let shim = GhShim::new();
    shim.fail_api_call(2);
    let failed = reason(push(&shim.backend(), "API_KEY"));
    assert!(failed.contains("may have landed"), "{failed}");
    assert_eq!(shim.stdin("API_KEY").as_deref(), Some(VALUE));
}

/// Why: a key GitHub refuses as a secret name would fail on GitHub's side;
/// it is refused before any spawn.
/// Test: itself.
#[test]
fn github_key_names_github_refuses_never_spawn() {
    let shim = GhShim::new();
    for name in ["a.b", "a-b", "1ABC", "GITHUB_TOKEN", "github_x"] {
        let refused = reason(push(&shim.backend(), name));
        assert!(refused.contains("[A-Za-z0-9_]"), "{name}: {refused}");
    }
    assert_eq!(shim.calls(), "", "nothing spawned");
    push(&shim.backend(), "lower_case").unwrap();
    assert!(
        shim.calls().contains("secret set LOWER_CASE "),
        "{}",
        shim.calls()
    );
}

/// Why: C12 — a push whose `gh` echoes the value to stdout and stderr, then
/// fails, yields an error and a `Debug` with neither the value nor the CLI's
/// text; a read-back and a delete are refused without spawning.
/// Test: itself.
#[test]
fn sync_target_errors_never_carry_the_value_or_cli_output() {
    let shim = GhShim::new();
    shim.echo();
    shim.fail_push("API_KEY");
    let backend = shim.backend();
    let failed = push(&backend, "API_KEY").unwrap_err();
    let text = format!("{failed} {failed:?} {backend:?}");
    assert!(
        !text.contains(VALUE) && !text.contains("HTTP 422"),
        "{text}"
    );

    let calls = shim.calls();
    assert!(matches!(
        backend.get(&web(), &key("API_KEY")),
        Err(SecretsError::Unsupported {
            operation: "get",
            ..
        })
    ));
    assert!(matches!(
        backend.delete(&web(), &key("API_KEY")),
        Err(SecretsError::Unsupported {
            operation: "delete",
            ..
        })
    ));
    assert_eq!(shim.calls(), calls, "no read-back or delete spawned");
    assert_eq!(
        backend.capabilities(),
        Capabilities::WRITE | Capabilities::SYNC_TARGET
    );
}

/// A machine config file under `dir` holding `yaml`.
fn machine_file(dir: &TempDir, yaml: &str) -> PathBuf {
    let path = dir.path().join("config.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

/// Why: ruling 74 — only the machine config's `secrets.github_actions:`
/// section enables a push; `default_backend` never does, and a missing file
/// or section refuses before anything is spawned.
/// Test: itself.
#[test]
fn github_open_requires_the_machine_section() {
    let shim = GhShim::new();
    let tmp = TempDir::new().unwrap();
    let dirs = [shim.program().parent().unwrap().to_path_buf()];
    let id = identity("acme", "web");
    let not_enabled = |yaml: &str| {
        let path = machine_file(&tmp, yaml);
        matches!(
            open_in(&path, &id, &dirs),
            Err(SecretsError::BackendNotEnabled { .. })
        )
    };
    assert!(not_enabled(""));
    assert!(not_enabled("secrets:\n  default_backend: github-actions\n"));
    assert!(not_enabled("secrets:\n  keeper: {}\n"));
    let missing = tmp.path().join("absent.yaml");
    assert!(matches!(
        open_in(&missing, &id, &dirs),
        Err(SecretsError::BackendNotEnabled { .. })
    ));

    let enabled = machine_file(&tmp, "secrets:\n  github_actions: {}\n");
    let backend = open_in(&enabled, &id, &dirs).unwrap();
    assert_eq!(backend.id(), BackendId::github_actions());
    assert_eq!(shim.calls(), "", "opening spawns nothing");
}

/// Why: ruling 74, D7 — the program is a machine pin or a fixed system
/// directory, never `PATH`; the config directory is absolute; an `account`
/// is refused. Each refusal comes before any spawn.
/// Test: itself.
#[test]
fn github_machine_settings_are_checked_before_any_spawn() {
    let shim = GhShim::new();
    let tmp = TempDir::new().unwrap();
    let id = identity("acme", "web");
    let open = |yaml: String, dirs: &[PathBuf]| {
        let path = machine_file(&tmp, &yaml);
        open_in(&path, &id, dirs)
    };
    let section = |body: &str| format!("secrets:\n  github_actions:\n{body}");
    let pin = format!("    program: {}\n", shim.program().display());

    let account = open(section(&format!("{pin}    account: someone\n")), &[]);
    assert!(
        matches!(account, Err(SecretsError::Config { .. })),
        "{account:?}"
    );
    let relative = open(section("    program: bin/gh\n"), &[]);
    assert!(
        matches!(relative, Err(SecretsError::Config { .. })),
        "{relative:?}"
    );
    let config = open(section(&format!("{pin}    config_path: ghconfig\n")), &[]);
    assert!(
        matches!(config, Err(SecretsError::Config { .. })),
        "{config:?}"
    );

    let plain = tmp.path().join("not-executable");
    std::fs::write(&plain, "").unwrap();
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
    let not_exec = open(section(&format!("    program: {}\n", plain.display())), &[]);
    assert!(
        matches!(not_exec, Err(SecretsError::CliNotInstalled { .. })),
        "{not_exec:?}"
    );
    let unpinned = open(
        section("    config_path: null\n"),
        &[tmp.path().to_path_buf()],
    );
    assert!(
        matches!(unpinned, Err(SecretsError::CliNotInstalled { .. })),
        "{unpinned:?}"
    );

    // The fixed directories, in order, when nothing is pinned.
    let other = install_script(&tmp.path().join("first"), "gh", "exit 0");
    let dirs = [
        other.parent().unwrap().to_path_buf(),
        shim.dir().join("bin"),
    ];
    let found = open(section("    config_path: null\n"), &dirs).unwrap();
    assert!(format!("{found:?}").contains(&other.display().to_string()));
    assert_eq!(shim.calls(), "", "nothing spawned");
}
