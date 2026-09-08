//! Unit tests for the per-account `gh` config directory bootstrap (#7166).
//!
//! Why: every function under test takes its paths as parameters, so these
//! run against real temp directories — no real `$HOME`, no real `gh`, no
//! network, and no process-global env mutation.
//! What: dir creation from an operator `hosts.yml` that names the login,
//! refusal (dir untouched) when it does not, reuse of an already-built dir,
//! `config.yml` copying (and tolerance of its absence), and the exact
//! `hosts.yml` shape written round-tripping through the real parser.
//! Test: this file IS the test module.

use super::*;

const TWO_ACCOUNT_HOSTS_YML: &str = "\
github.com:
    git_protocol: https
    users:
        bobmatnyc:
        bob-duetto:
    user: bob-duetto
";

/// Build a fake operator gh config dir (`hosts.yml` + optional `config.yml`)
/// under `root`, returning its path.
fn fake_operator_gh_config_dir(
    root: &std::path::Path,
    hosts_yml: &str,
    config_yml: Option<&str>,
) -> std::path::PathBuf {
    let dir = root.join("operator-gh-config");
    std::fs::create_dir_all(&dir).expect("create fake operator gh config dir");
    std::fs::write(dir.join("hosts.yml"), hosts_yml).expect("write fake hosts.yml");
    if let Some(config) = config_yml {
        std::fs::write(dir.join("config.yml"), config).expect("write fake config.yml");
    }
    dir
}

#[test]
fn ensure_account_config_dir_places_it_under_gh_accounts() {
    let state_root = std::path::Path::new("/tmp/tm-state");
    assert_eq!(
        account_config_dir(state_root, "bob-duetto"),
        std::path::PathBuf::from("/tmp/tm-state/gh-accounts/bob-duetto")
    );
}

/// The headline case: a login the operator's `hosts.yml` names gets a fresh,
/// isolated config dir built for it.
#[test]
fn ensure_account_config_dir_builds_from_operator_hosts_yml() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_root = tmp.path().join("state");
    let operator_dir = fake_operator_gh_config_dir(tmp.path(), TWO_ACCOUNT_HOSTS_YML, None);

    let dir = ensure_account_config_dir(&state_root, &operator_dir, "bob-duetto")
        .expect("bob-duetto is a known account");

    assert_eq!(dir, state_root.join("gh-accounts").join("bob-duetto"));
    assert!(dir.join("hosts.yml").is_file());
    let written = std::fs::read_to_string(dir.join("hosts.yml")).unwrap();
    assert!(written.contains("bob-duetto"), "{written}");
    assert!(
        !written.contains("bobmatnyc"),
        "the built hosts.yml must name ONLY the selected account: {written}"
    );
    // No token was ever written anywhere this function touches.
    assert!(!written.to_lowercase().contains("token"), "{written}");
}

/// The login is matched case-insensitively, but the WRITTEN file uses `gh`'s
/// own canonical spelling — not whatever case the caller passed.
#[test]
fn ensure_account_config_dir_uses_the_canonical_spelling() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_root = tmp.path().join("state");
    let operator_dir = fake_operator_gh_config_dir(tmp.path(), TWO_ACCOUNT_HOSTS_YML, None);

    // Note the deliberately wrong case on input.
    let dir = ensure_account_config_dir(&state_root, &operator_dir, "BOB-DUETTO")
        .expect("case-insensitive match must succeed");

    // The directory path itself is keyed on the CALLER's spelling (matches
    // register_args'/the CLI's own case handling of --account), but the
    // hosts.yml content inside it is gh's canonical spelling.
    assert_eq!(dir, state_root.join("gh-accounts").join("BOB-DUETTO"));
    let written = std::fs::read_to_string(dir.join("hosts.yml")).unwrap();
    assert!(written.contains("bob-duetto"), "{written}");
}

/// A login the operator's `hosts.yml` does NOT name is refused, with the
/// `gh auth login` remedy, and the directory is never created.
#[test]
fn ensure_account_config_dir_refuses_an_unknown_login() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_root = tmp.path().join("state");
    let operator_dir = fake_operator_gh_config_dir(tmp.path(), TWO_ACCOUNT_HOSTS_YML, None);

    let err = ensure_account_config_dir(&state_root, &operator_dir, "someone-else")
        .expect_err("an unlogged-in account must be refused");
    assert!(err.contains("someone-else"), "{err}");
    assert!(err.contains("gh auth login"), "{err}");
    assert!(
        !state_root.join("gh-accounts").join("someone-else").exists(),
        "a refused account must never get a directory"
    );
}

/// No operator `hosts.yml` at all (never ran `gh auth login`) is the same
/// refusal shape, not a panic or a different error class.
#[test]
fn ensure_account_config_dir_refuses_when_operator_has_no_hosts_yml() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_root = tmp.path().join("state");
    let operator_dir = tmp.path().join("empty-operator-dir");
    std::fs::create_dir_all(&operator_dir).unwrap();

    let err = ensure_account_config_dir(&state_root, &operator_dir, "bob-duetto")
        .expect_err("no hosts.yml at all must refuse");
    assert!(err.contains("gh auth login"), "{err}");
}

/// An already-built dir (from a prior `--account` use) is reused UNTOUCHED —
/// no re-read of the operator's `hosts.yml`, no rewrite.
#[test]
fn ensure_account_config_dir_reuses_an_existing_dir_untouched() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_root = tmp.path().join("state");
    let operator_dir = fake_operator_gh_config_dir(tmp.path(), TWO_ACCOUNT_HOSTS_YML, None);

    let dir = ensure_account_config_dir(&state_root, &operator_dir, "bob-duetto")
        .expect("first build succeeds");
    let sentinel = "hand-edited-by-the-operator, must survive reuse";
    std::fs::write(dir.join("hosts.yml"), sentinel).unwrap();

    // Second call: even with an operator hosts.yml that no longer lists the
    // account at all, an already-built dir must still be reused as-is.
    let empty_operator_dir = tmp.path().join("now-empty-operator-dir");
    std::fs::create_dir_all(&empty_operator_dir).unwrap();
    let dir2 = ensure_account_config_dir(&state_root, &empty_operator_dir, "bob-duetto")
        .expect("an existing dir must be reused, not rebuilt or refused");

    assert_eq!(dir, dir2);
    assert_eq!(
        std::fs::read_to_string(dir.join("hosts.yml")).unwrap(),
        sentinel,
        "reuse must not overwrite the existing hosts.yml"
    );
}

/// The operator's `config.yml` is copied into the new account dir when present.
#[test]
fn ensure_account_config_dir_copies_config_yml_when_present() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_root = tmp.path().join("state");
    let operator_dir = fake_operator_gh_config_dir(
        tmp.path(),
        TWO_ACCOUNT_HOSTS_YML,
        Some("git_protocol: ssh\n"),
    );

    let dir = ensure_account_config_dir(&state_root, &operator_dir, "bob-duetto").unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("config.yml")).unwrap(),
        "git_protocol: ssh\n"
    );
}

/// A missing `config.yml` on the operator side is NOT an error — `gh`
/// tolerates a config dir with no `config.yml` of its own.
#[test]
fn ensure_account_config_dir_tolerates_a_missing_config_yml() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_root = tmp.path().join("state");
    let operator_dir = fake_operator_gh_config_dir(tmp.path(), TWO_ACCOUNT_HOSTS_YML, None);

    let dir = ensure_account_config_dir(&state_root, &operator_dir, "bob-duetto")
        .expect("no config.yml must not be an error");
    assert!(!dir.join("config.yml").exists());
}

/// The exact `hosts.yml` this module writes round-trips through the real
/// `gh_account` parser and names exactly one account.
#[test]
fn render_single_account_hosts_yml_round_trips_through_the_parser() {
    let text = render_single_account_hosts_yml("bob-duetto");
    let status =
        crate::core::gh_account::parse_gh_account_status_from_hosts_yml(&text).expect("must parse");
    assert_eq!(status.active.as_deref(), Some("bob-duetto"));
    assert_eq!(status.logged_in, vec!["bob-duetto".to_string()]);
    assert!(!status.is_ambiguous());
}

// -----------------------------------------------------------------------
// #7166 review follow-up MEDIUM: `login` must be rejected before it ever
// becomes a path segment — `is_name_segment` (the CLI-layer check) restricts
// only the character set, not the exact strings `.`/`..`.
// -----------------------------------------------------------------------

#[test]
fn ensure_account_config_dir_refuses_dot() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_root = tmp.path().join("state");
    let operator_dir = fake_operator_gh_config_dir(tmp.path(), TWO_ACCOUNT_HOSTS_YML, None);

    let err = ensure_account_config_dir(&state_root, &operator_dir, ".")
        .expect_err("'.' must be refused before any join");
    assert!(err.contains('.'), "{err}");
    assert!(
        !state_root.exists(),
        "a refused login must never touch the filesystem"
    );
}

#[test]
fn ensure_account_config_dir_refuses_dotdot() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_root = tmp.path().join("state");
    let operator_dir = fake_operator_gh_config_dir(tmp.path(), TWO_ACCOUNT_HOSTS_YML, None);

    let err = ensure_account_config_dir(&state_root, &operator_dir, "..").expect_err(
        "'..' must be refused before any join — otherwise it resolves to state_root itself",
    );
    assert!(err.contains(".."), "{err}");
}

#[test]
fn ensure_account_config_dir_refuses_empty() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_root = tmp.path().join("state");
    let operator_dir = fake_operator_gh_config_dir(tmp.path(), TWO_ACCOUNT_HOSTS_YML, None);

    let err = ensure_account_config_dir(&state_root, &operator_dir, "")
        .expect_err("an empty login must be refused");
    assert!(err.contains("not a valid account login"), "{err}");
}

#[test]
fn ensure_account_config_dir_refuses_a_forward_slash() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_root = tmp.path().join("state");
    let operator_dir = fake_operator_gh_config_dir(tmp.path(), TWO_ACCOUNT_HOSTS_YML, None);

    let err = ensure_account_config_dir(&state_root, &operator_dir, "bob/duetto")
        .expect_err("a path separator must be refused");
    assert!(err.contains("bob/duetto"), "{err}");
}

#[test]
fn ensure_account_config_dir_refuses_a_backslash() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_root = tmp.path().join("state");
    let operator_dir = fake_operator_gh_config_dir(tmp.path(), TWO_ACCOUNT_HOSTS_YML, None);

    let err = ensure_account_config_dir(&state_root, &operator_dir, "bob\\duetto")
        .expect_err("a path separator must be refused");
    assert!(err.contains("bob\\duetto"), "{err}");
}

// -----------------------------------------------------------------------
// #7166 review follow-up LOW: a pre-planted symlink at the dir or at
// `hosts.yml` must be refused before any create/write follows it.
// -----------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn ensure_account_config_dir_refuses_a_symlinked_dir() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_root = tmp.path().join("state");
    let operator_dir = fake_operator_gh_config_dir(tmp.path(), TWO_ACCOUNT_HOSTS_YML, None);

    let real_target = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&real_target).unwrap();
    std::fs::create_dir_all(state_root.join("gh-accounts")).unwrap();
    std::os::unix::fs::symlink(
        &real_target,
        state_root.join("gh-accounts").join("bob-duetto"),
    )
    .expect("create symlink");

    let err = ensure_account_config_dir(&state_root, &operator_dir, "bob-duetto")
        .expect_err("a pre-planted symlinked dir must be refused");
    assert!(err.contains("symlink"), "{err}");
}

#[test]
#[cfg(unix)]
fn ensure_account_config_dir_refuses_a_symlinked_hosts_yml() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_root = tmp.path().join("state");
    let operator_dir = fake_operator_gh_config_dir(tmp.path(), TWO_ACCOUNT_HOSTS_YML, None);

    let dir = state_root.join("gh-accounts").join("bob-duetto");
    std::fs::create_dir_all(&dir).unwrap();
    let real_target = tmp.path().join("some-real-file");
    std::fs::write(&real_target, "not a hosts.yml").unwrap();
    std::os::unix::fs::symlink(&real_target, dir.join("hosts.yml")).expect("create symlink");

    let err = ensure_account_config_dir(&state_root, &operator_dir, "bob-duetto")
        .expect_err("a pre-planted symlinked hosts.yml must be refused");
    assert!(err.contains("symlink"), "{err}");
}

// -----------------------------------------------------------------------
// #7166 review follow-up MEDIUM: the built directory and files must be
// owner-only (0700/0600), matching `gh`'s own `hosts.yml` convention.
// -----------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn ensure_account_config_dir_sets_restrictive_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_root = tmp.path().join("state");
    let operator_dir = fake_operator_gh_config_dir(
        tmp.path(),
        TWO_ACCOUNT_HOSTS_YML,
        Some("git_protocol: ssh\n"),
    );

    let dir = ensure_account_config_dir(&state_root, &operator_dir, "bob-duetto").unwrap();

    let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "dir mode was {dir_mode:o}");

    let hosts_mode = std::fs::metadata(dir.join("hosts.yml"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(hosts_mode, 0o600, "hosts.yml mode was {hosts_mode:o}");

    let config_mode = std::fs::metadata(dir.join("config.yml"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(config_mode, 0o600, "config.yml mode was {config_mode:o}");
}
