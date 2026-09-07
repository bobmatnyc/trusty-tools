//! Unit tests for the `.trusty-code/` project-configuration resolver (#5426).
//!
//! Why: the precedence rule, the fail-open warning, and the native-write guard
//! are the three properties every other #5426 change depends on; each is proven
//! here against a real temp tree rather than a mocked filesystem.
//! What: precedence across the three search roots, the default when nothing
//! exists, the unreadable-candidate fallback (directory AND file), the
//! write-target guard including a symlink escape, and the secret-key scanner.
//! Test: this file IS the test module.

use super::*;

use std::sync::{Arc, Mutex};

/// A `tracing` writer collecting every formatted event into a shared buffer.
///
/// Why: the Fail-Open Check requires the fallback to LOG at warn with the path
/// tried; asserting on the returned `unreadable` field alone would pass even if
/// the warning were deleted.
/// What: a `MakeWriter` over an `Arc<Mutex<Vec<u8>>>`, installed per-test with
/// `tracing::subscriber::with_default` (thread-local, so parallel tests in this
/// binary never race a global subscriber).
/// Test: used by `unreadable_native_dir_falls_back_and_is_reported`.
#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Run `f` with a thread-local tracing subscriber and return its captured output.
fn capture_logs<T>(f: impl FnOnce() -> T) -> (T, String) {
    let buffer = Arc::new(Mutex::new(Vec::<u8>::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(CaptureWriter(Arc::clone(&buffer)))
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let logs =
        String::from_utf8_lossy(&buffer.lock().unwrap_or_else(|e| e.into_inner())).to_string();
    (out, logs)
}

/// Create `<root>/<dirname>/<relative>` as a directory.
fn mkdir(root: &std::path::Path, dirname: &str, relative: &str) -> PathBuf {
    let p = root.join(dirname).join(relative);
    std::fs::create_dir_all(&p).expect("mkdir");
    p
}

/// Whether this process runs as root (which ignores permission bits).
#[cfg(unix)]
fn running_as_root() -> bool {
    // SAFETY: `geteuid` takes no arguments, touches no memory, and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

// -- precedence ------------------------------------------------------------

/// `.trusty-code/` wins over `.claude/` when both exist.
///
/// Why: this is the whole inversion #5426 asks for — Trusty Code's own
/// directory is the source of truth, and `.claude/` only fills in behind it.
/// What: creates both `agents` directories and asserts the resolved path and
/// source name the native one.
/// Test: this function IS the test.
#[test]
fn precedence_prefers_trusty_code_over_claude() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let native = mkdir(tmp.path(), TRUSTY_CODE_DIRNAME, "agents");
    mkdir(tmp.path(), CLAUDE_COMPAT_DIRNAME, "agents");

    let resolved = agents_dir(tmp.path());

    assert_eq!(resolved.path, native);
    assert_eq!(resolved.source, ConfigSource::TrustyCode);
    assert!(resolved.unreadable.is_empty());
}

/// `.claude/` still resolves when `.trusty-code/` is absent.
///
/// Why: every existing project has only `.claude/`; breaking them was never on
/// the table.
/// What: creates only `.claude/agents` and asserts it wins with the
/// compatibility source.
/// Test: this function IS the test.
#[test]
fn falls_back_to_claude_when_trusty_code_absent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let claude = mkdir(tmp.path(), CLAUDE_COMPAT_DIRNAME, "agents");

    let resolved = agents_dir(tmp.path());

    assert_eq!(resolved.path, claude);
    assert_eq!(resolved.source, ConfigSource::ClaudeCompat);
    assert!(!resolved.source.is_native());
}

/// `.open-mpm/` is still honoured behind both newer roots.
///
/// Why: `agents::locate_agents_dir` already supported it; the rewrite must not
/// quietly drop a supported layout.
/// What: creates only `.open-mpm/agents` and asserts it wins.
/// Test: this function IS the test.
#[test]
fn falls_back_to_open_mpm_when_neither_native_nor_claude() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let legacy = mkdir(tmp.path(), OPEN_MPM_LEGACY_DIRNAME, "agents");

    let resolved = agents_dir(tmp.path());

    assert_eq!(resolved.path, legacy);
    assert_eq!(resolved.source, ConfigSource::OpenMpmLegacy);
}

/// With nothing on disk, the resolver names the NATIVE path.
///
/// Why: "a clean project can install and run without a `.claude/` directory"
/// requires that the not-yet-created default point at `.trusty-code/`, not at
/// the compatibility root.
/// What: an empty project root; asserts the default path and source.
/// Test: this function IS the test.
#[test]
fn default_source_when_nothing_exists() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let resolved = agents_dir(tmp.path());

    assert_eq!(
        resolved.path,
        tmp.path().join(TRUSTY_CODE_DIRNAME).join("agents")
    );
    assert_eq!(resolved.source, ConfigSource::Default);
    assert!(resolved.source.is_native());
}

/// Skills, plugins, and settings follow the same precedence as agents.
///
/// Why: the point of one resolver is that no entry has its own rule; a per-kind
/// test is what proves the wiring actually routes through it.
/// What: creates each native entry alongside a `.claude/` twin and asserts the
/// native one wins in all three.
/// Test: this function IS the test.
#[test]
fn skills_dir_prefers_trusty_code() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let native = mkdir(tmp.path(), TRUSTY_CODE_DIRNAME, "skills");
    mkdir(tmp.path(), CLAUDE_COMPAT_DIRNAME, "skills");
    assert_eq!(skills_dir(tmp.path()).path, native);
}

/// See `skills_dir_prefers_trusty_code`.
#[test]
fn plugins_dir_prefers_trusty_code() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let native = mkdir(tmp.path(), TRUSTY_CODE_DIRNAME, "plugins");
    mkdir(tmp.path(), CLAUDE_COMPAT_DIRNAME, "plugins");
    assert_eq!(plugins_dir(tmp.path()).path, native);
}

/// See `skills_dir_prefers_trusty_code`.
#[test]
fn agents_dir_prefers_trusty_code() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let native = mkdir(tmp.path(), TRUSTY_CODE_DIRNAME, "agents");
    mkdir(tmp.path(), CLAUDE_COMPAT_DIRNAME, "agents");
    assert_eq!(agents_dir(tmp.path()).path, native);
}

/// `settings.json` resolves as a FILE, native first.
///
/// Why: a directory named `settings.json` must not satisfy the lookup, and a
/// project with only `.trusty-code/settings.json` must be able to set
/// `code_harness.mode`.
/// What: writes both files and asserts the native one wins.
/// Test: this function IS the test.
#[test]
fn settings_file_prefers_trusty_code() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let native_dir = mkdir(tmp.path(), TRUSTY_CODE_DIRNAME, "");
    let claude_dir = mkdir(tmp.path(), CLAUDE_COMPAT_DIRNAME, "");
    std::fs::write(native_dir.join(SETTINGS_FILENAME), "{}").expect("write");
    std::fs::write(claude_dir.join(SETTINGS_FILENAME), "{}").expect("write");

    let resolved = settings_file(tmp.path());

    assert_eq!(resolved.path, native_dir.join(SETTINGS_FILENAME));
    assert_eq!(resolved.source, ConfigSource::TrustyCode);
}

/// The source tokens are the wire contract the CLI and logs share.
#[test]
fn source_tokens_are_stable() {
    assert_eq!(ConfigSource::TrustyCode.as_str(), "trusty-code");
    assert_eq!(ConfigSource::ClaudeCompat.as_str(), "claude");
    assert_eq!(ConfigSource::OpenMpmLegacy.as_str(), "open-mpm");
    assert_eq!(ConfigSource::Default.as_str(), "default");
    assert!(ConfigSource::TrustyCode.is_native());
    assert!(ConfigSource::Default.is_native());
    assert!(!ConfigSource::ClaudeCompat.is_native());
    assert!(!ConfigSource::OpenMpmLegacy.is_native());
}

// -- fail-open behaviour ---------------------------------------------------

/// An unreadable `.trusty-code/agents` falls back to `.claude/agents`, records
/// the skipped path, and WARNS with it.
///
/// Why: the Fail-Open Check. A harness that aborts because an optional config
/// directory lost its read bit is worse than one that starts with less config —
/// but a silent fallback is worse than both, because the operator then debugs
/// the wrong directory.
/// What: chmods `.trusty-code/agents` to `0o000`, resolves under a captured
/// subscriber, and asserts the winner is `.claude`, the skipped path is
/// reported, and the warning names the unreadable path.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn unreadable_native_dir_falls_back_and_is_reported() {
    use std::os::unix::fs::PermissionsExt;

    if running_as_root() {
        // root ignores permission bits, so the unreadable case cannot be staged.
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let native = mkdir(tmp.path(), TRUSTY_CODE_DIRNAME, "agents");
    let claude = mkdir(tmp.path(), CLAUDE_COMPAT_DIRNAME, "agents");
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o000)).expect("chmod");

    let (resolved, logs) = capture_logs(|| agents_dir(tmp.path()));

    // Restore before any assertion can unwind past the tempdir cleanup.
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o755)).expect("restore");

    assert_eq!(resolved.source, ConfigSource::ClaudeCompat);
    assert_eq!(resolved.path, claude);
    assert_eq!(resolved.unreadable, vec![native.clone()]);
    assert!(
        logs.contains(&native.display().to_string()),
        "warning must name the unreadable path tried; logs were:\n{logs}"
    );
    assert!(
        logs.contains("WARN"),
        "fallback must be logged at warn; logs were:\n{logs}"
    );
}

/// An unreadable `settings.json` falls back to the next root and WARNS.
///
/// Why: the file-shaped half of the same Fail-Open Check — `probe` uses a
/// different syscall for files, so the directory test does not cover it.
/// What: chmods `.trusty-code/settings.json` to `0o000` and asserts the
/// `.claude/` one wins with a warning naming the unreadable path.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn unreadable_settings_file_falls_back_and_is_reported() {
    use std::os::unix::fs::PermissionsExt;

    if running_as_root() {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let native_dir = mkdir(tmp.path(), TRUSTY_CODE_DIRNAME, "");
    let claude_dir = mkdir(tmp.path(), CLAUDE_COMPAT_DIRNAME, "");
    let native = native_dir.join(SETTINGS_FILENAME);
    std::fs::write(&native, "{}").expect("write");
    std::fs::write(claude_dir.join(SETTINGS_FILENAME), "{}").expect("write");
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o000)).expect("chmod");

    let (resolved, logs) = capture_logs(|| settings_file(tmp.path()));

    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o644)).expect("restore");

    assert_eq!(resolved.source, ConfigSource::ClaudeCompat);
    assert_eq!(resolved.unreadable, vec![native.clone()]);
    assert!(
        logs.contains(&native.display().to_string()),
        "warning must name the unreadable path tried; logs were:\n{logs}"
    );
}

// -- native write boundary -------------------------------------------------

/// `native_config_dir` never consults the filesystem.
///
/// Why: the read path may resolve into `.claude/`; the WRITE path must not, even
/// on a project where only `.claude/` exists.
/// What: an empty root and a `.claude`-only root both yield `.trusty-code`.
/// Test: this function IS the test.
#[test]
fn native_config_dir_is_always_trusty_code() {
    let tmp = tempfile::tempdir().expect("tempdir");
    mkdir(tmp.path(), CLAUDE_COMPAT_DIRNAME, "agents");

    assert_eq!(
        native_config_dir(tmp.path()),
        tmp.path().join(TRUSTY_CODE_DIRNAME)
    );
    assert_eq!(
        native_child(tmp.path(), "agents/pm.md"),
        tmp.path().join(TRUSTY_CODE_DIRNAME).join("agents/pm.md")
    );
}

/// A target beneath `.trusty-code/` is permitted.
#[test]
fn write_to_native_dir_is_allowed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = native_child(tmp.path(), "agents/pm.md");
    assert_eq!(
        check_native_write_target(tmp.path(), &target).expect("native target must be allowed"),
        target
    );
}

/// A write aimed at `.claude/` is refused as a cross-product write.
///
/// Why: "native project writes stay beneath `<project>/.trusty-code/`" — a
/// harness that writes into Claude Code's directory is mutating another
/// product's state.
/// What: asserts a `.claude/agents/pm.md` target yields `CrossProduct`.
/// Test: this function IS the test.
#[test]
fn write_to_claude_dir_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join(CLAUDE_COMPAT_DIRNAME).join("agents/pm.md");

    let err = check_native_write_target(tmp.path(), &target).expect_err("must be refused");

    assert!(matches!(err, WriteTargetError::CrossProduct { .. }));
    assert!(err.to_string().contains(TRUSTY_CODE_DIRNAME));
}

/// A write outside the project entirely is refused.
#[test]
fn write_outside_project_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let err = check_native_write_target(tmp.path(), std::path::Path::new("/etc/passwd"))
        .expect_err("must be refused");
    assert!(matches!(err, WriteTargetError::CrossProduct { .. }));
}

/// A lexically-inside target that resolves outside through a symlink is refused.
///
/// Why: `starts_with` alone is satisfied by `.trusty-code/agents ->
/// /somewhere/else`, and every write would then land outside the project.
/// What: symlinks `.trusty-code/agents` at a sibling temp directory and asserts
/// a target inside it is refused as a `SymlinkEscape`.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn symlinked_native_subdir_escape_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("tempdir");
    let native = native_config_dir(tmp.path());
    std::fs::create_dir_all(&native).expect("mkdir");
    std::os::unix::fs::symlink(outside.path(), native.join("agents")).expect("symlink");

    let target = native.join("agents").join("pm.md");
    let err = check_native_write_target(tmp.path(), &target).expect_err("must be refused");

    assert!(
        matches!(err, WriteTargetError::SymlinkEscape { .. }),
        "expected SymlinkEscape, got {err:?}"
    );
}

// -- secret scanning -------------------------------------------------------

/// Secret-bearing keys are found anywhere in the JSON tree, by dotted path.
#[test]
fn secret_bearing_keys_are_detected() {
    let value = serde_json::json!({
        "code_harness": { "mode": "parity" },
        "mcp": [{ "name": "x", "env": { "OPENROUTER_API_KEY": "sk-live" } }],
    });
    assert_eq!(
        find_secret_key(&value).as_deref(),
        Some("mcp.0.env.OPENROUTER_API_KEY")
    );

    let top = serde_json::json!({ "authToken": "abc" });
    assert_eq!(find_secret_key(&top).as_deref(), Some("authToken"));
}

/// An ordinary settings file carries no secret.
#[test]
fn ordinary_settings_carry_no_secret() {
    let value = serde_json::json!({ "code_harness": { "mode": "daily-driver" } });
    assert_eq!(find_secret_key(&value), None);
}

/// Kebab, camel, header and SCREAMING spellings of the same key all match.
///
/// Why: code-critic HIGH on PR #6980 — the scanner matched raw lowercase
/// substrings, so `api_key` was caught and `API-KEY`, `x-api-key`, `access-key`
/// and `private-key` were not. A credential under any of those spellings
/// imported straight into a file the project commits.
/// What: one key per spelling, each asserted secret-bearing.
/// Test: this function IS the test.
#[test]
fn secret_keys_match_across_separator_and_case_spellings() {
    for key in [
        "api_key",
        "API-KEY",
        "x-api-key",
        "xApiKey",
        "apiKey",
        "APIKEY",
        "access-key",
        "accessKey",
        "private-key",
        "AWS_SECRET_ACCESS_KEY",
        "Authorization",
        "auth-token",
        "authToken",
        "user.password",
        "PassPhrase",
        "gh-credential",
    ] {
        assert!(
            is_secret_key(key),
            "{key} must be recognised as secret-bearing"
        );
    }
}

/// A benign key that merely CONTAINS a hint is not flagged.
///
/// Why: the fix for the spelling gap must not be a looser substring match —
/// `tokenizer` and `secretary` contain `token` and `secret` and are ordinary
/// configuration, so flagging them would refuse imports for no reason.
/// What: asserts each benign key is clean, including a compound where the hint
/// is a strict prefix or suffix of a real word.
/// Test: this function IS the test.
#[test]
fn benign_keys_containing_a_hint_are_not_flagged() {
    for key in [
        "tokenizer",
        "tokenizerPath",
        "secretary",
        "max_tokens",
        "keychain",
        "keyboard",
        "passwordless_hint_text",
        "mode",
        "cadence_turns",
    ] {
        assert!(!is_secret_key(key), "{key} must NOT be flagged as secret");
    }
}

/// `find_secret_key` reports the dotted path of a kebab-spelled key.
///
/// Why: the walk and the per-key rule are separate; proving the rule alone would
/// not show the walk actually applies it at depth.
/// What: a header-style key nested under an array element.
/// Test: this function IS the test.
#[test]
fn nested_kebab_secret_is_reported_by_path() {
    let value = serde_json::json!({
        "mcp": [{ "name": "x", "headers": { "X-Api-Key": "sk-live" } }],
    });
    assert_eq!(
        find_secret_key(&value).as_deref(),
        Some("mcp.0.headers.X-Api-Key")
    );
}

/// A `.trusty-code` that is ITSELF a symlink out of the project is refused.
///
/// Why: code-critic BLOCK on PR #6980. The guard resolved the target and the
/// config root the same way and compared them to EACH OTHER, so when
/// `<project>/.trusty-code` pointed at an outside directory, every "protected"
/// write agreed with a root that was already outside the project and passed.
/// `symlinked_native_subdir_escape_is_refused` covers a symlinked SUBdirectory
/// and does not reach this case.
/// What: symlinks `<project>/.trusty-code` at a sibling temp directory and
/// asserts a target inside it is refused as a `SymlinkEscape`.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn symlinked_native_root_escape_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("tempdir");
    std::os::unix::fs::symlink(outside.path(), tmp.path().join(TRUSTY_CODE_DIRNAME))
        .expect("symlink");

    let target = native_child(tmp.path(), "agents/pm.md");
    let err = check_native_write_target(tmp.path(), &target).expect_err("must be refused");

    assert!(
        matches!(err, WriteTargetError::SymlinkEscape { .. }),
        "expected SymlinkEscape, got {err:?}"
    );
}
