//! Unit tests for the canonical palace resolver (#5811).
//!
//! Why: the defect this module fixes was a precedence question that no test
//! could previously ask, because every level returned a bare `String`. These
//! tests assert the deciding level, not just the value.
//!
//! Tests that mutate `TRUSTY_MEMORY_PALACE` are `#[serial]` — the variable is
//! process-global and several other suites in this crate read it.

use super::*;
use std::fs;

/// Write a valid pin file under `root`, creating `.trusty-tools/`.
fn write_pin(root: &Path, palace: &str) -> PathBuf {
    let dir = root.join(TRUSTY_TOOLS_DIR);
    fs::create_dir_all(&dir).expect("create .trusty-tools");
    let path = root.join(PIN_FILE_REL);
    let pin = ProjectPin::new(palace);
    fs::write(&path, serde_yaml::to_string(&pin).expect("serialise")).expect("write pin");
    path
}

/// Initialise a real git repo at `root` with `remote.origin.url` set.
///
/// Returns `false` when git is unavailable, so the caller can skip rather than
/// fail on a machine without git.
fn init_repo(root: &Path, remote: Option<&str>) -> bool {
    let ok = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !ok(&["init", "-q"]) {
        return false;
    }
    // Local identity so `commit` works on a machine with no global config.
    ok(&["config", "user.email", "t@example.com"]);
    ok(&["config", "user.name", "T"]);
    if let Some(url) = remote {
        ok(&["config", "remote.origin.url", url]);
    }
    true
}

// ---------------------------------------------------------------------------
// find_project_root
// ---------------------------------------------------------------------------

/// Why: the pin lives at the project root, so a call from a nested directory
/// must still find it.
#[test]
fn finds_git_root_from_nested_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("proj");
    fs::create_dir_all(root.join(".git")).unwrap();
    let nested = root.join("crates").join("foo");
    fs::create_dir_all(&nested).unwrap();

    let found = find_project_root(&nested).expect("root");
    assert_eq!(found, fs::canonicalize(&root).unwrap());
}

/// Why: a directory carrying only a pin file must still count as a project.
#[test]
fn trusty_tools_dir_is_a_marker() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("proj");
    fs::create_dir_all(root.join(TRUSTY_TOOLS_DIR)).unwrap();
    assert!(find_project_root(&root).is_some());
}

/// Why: outside any project the caller must be told so, not handed a guess.
#[test]
fn no_markers_returns_none() {
    // A bare tempdir has no markers, but its ANCESTORS might (e.g. `/tmp` on a
    // machine where something dropped a marker). Assert on a synthetic path
    // that cannot exist instead.
    let missing = Path::new("/nonexistent-trusty-test-root-5802/deeper");
    assert!(find_project_root(missing).is_none());
}

// ---------------------------------------------------------------------------
// read_project_pin — the fail-closed contract
// ---------------------------------------------------------------------------

/// Why: the happy path — a well-formed pin parses to its palace.
#[test]
fn reads_a_valid_pin() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_pin(tmp.path(), "canonical-name");
    let pin = read_project_pin(tmp.path()).expect("ok").expect("some");
    assert_eq!(pin.palace, "canonical-name");
}

/// Why: "no pin" is the normal case and must not be an error — it is what lets
/// an unpinned project fall through to derivation.
#[test]
fn absent_pin_is_ok_none() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(read_project_pin(tmp.path()).expect("ok").is_none());
}

/// Why: THE fail-open regression (#5811). A pin file that exists but does not
/// parse used to log a warning and fall through to git derivation, handing the
/// caller a plausible name for a palace nobody chose. Against the pre-fix
/// `trusty_memory::project_root::project_slug_at`, this input returned
/// `Some("<basename>")`; it must now be an error.
#[test]
fn malformed_pin_is_an_error_not_a_fallthrough() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("some-project");
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join(TRUSTY_TOOLS_DIR)).unwrap();
    // Valid YAML, wrong shape — `palace` is a map where a string is required.
    fs::write(
        root.join(PIN_FILE_REL),
        "schema_version: 1\npalace:\n  not: a-string\n",
    )
    .unwrap();

    let err = read_project_pin(&root).expect_err("malformed pin must error");
    assert!(
        matches!(err, PalaceResolveError::PinMalformed { .. }),
        "expected PinMalformed, got {err:?}"
    );

    // And the full resolver must propagate rather than derive past it.
    let err = resolve_palace(&root).expect_err("resolver must not fall through");
    assert!(
        matches!(err, PalaceResolveError::PinMalformed { .. }),
        "expected PinMalformed from resolve_palace, got {err:?}"
    );
}

/// Why: a pin whose `palace` field is present but blank is equally
/// untrustworthy — it names no palace, and deriving past it silently redirects
/// writes.
#[test]
fn empty_pin_palace_is_an_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("proj");
    fs::create_dir_all(root.join(".git")).unwrap();
    write_pin(&root, "   ");

    let err = resolve_palace(&root).expect_err("empty pin must error");
    assert!(
        matches!(err, PalaceResolveError::PinEmpty { .. }),
        "expected PinEmpty, got {err:?}"
    );
}

/// Why: an unreadable pin (permissions) is the third untrustworthy case and
/// must not degrade to derivation either.
#[cfg(unix)]
#[test]
fn unreadable_pin_is_an_error() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("proj");
    fs::create_dir_all(root.join(".git")).unwrap();
    let path = write_pin(&root, "pinned");

    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&path, perms).unwrap();

    let result = read_project_pin(&root);

    // Restore before asserting so the tempdir can always be cleaned up.
    let mut restore = fs::metadata(&path).unwrap().permissions();
    restore.set_mode(0o644);
    fs::set_permissions(&path, restore).unwrap();

    // Running as root defeats the permission bit entirely; skip rather than
    // assert a guarantee the environment cannot provide.
    if let Ok(Some(_)) = result {
        eprintln!("skipping unreadable_pin_is_an_error: running with root privileges");
        return;
    }
    let err = result.expect_err("unreadable pin must error");
    assert!(
        matches!(err, PalaceResolveError::PinUnreadable { .. }),
        "expected PinUnreadable, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Precedence
// ---------------------------------------------------------------------------

/// Why: the core of the reported symptom. A committed pin must beat the git
/// `owner/repo` derivation — the pin exists precisely to stop the derived name
/// from orphaning memories written under the pinned one.
#[test]
#[serial_test::serial]
fn pin_beats_git_derivation() {
    let _guard = EnvGuard::clear();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("checkout-dir");
    fs::create_dir_all(&root).unwrap();
    if !init_repo(&root, Some("git@github.com:bobmatnyc/trusty-tools.git")) {
        eprintln!("skipping pin_beats_git_derivation: git unavailable");
        return;
    }
    write_pin(&root, "trusty-tools");

    let got = resolve_palace(&root).expect("resolves");
    assert_eq!(got.id, "trusty-tools");
    assert_eq!(got.source, PalaceSource::PinFile);
}

/// Why: without a pin the git identity decides, and it is hyphenated so it is
/// safe as a directory name and a socket filename.
#[test]
#[serial_test::serial]
fn git_owner_repo_used_when_unpinned() {
    let _guard = EnvGuard::clear();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("checkout-dir");
    fs::create_dir_all(&root).unwrap();
    if !init_repo(&root, Some("git@github.com:bobmatnyc/trusty-tools.git")) {
        eprintln!("skipping git_owner_repo_used_when_unpinned: git unavailable");
        return;
    }

    let got = resolve_palace(&root).expect("resolves");
    assert_eq!(got.id, "bobmatnyc-trusty-tools");
    assert_eq!(got.source, PalaceSource::GitOwnerRepo);
}

/// Why: the operator escape hatch must keep working — it is how CI and test
/// rigs pin a palace — and its precedence must be deterministic and reported,
/// so a producer that launders a derived value through it is diagnosable.
#[test]
#[serial_test::serial]
fn env_override_wins_over_pin_and_warns() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("proj");
    fs::create_dir_all(root.join(".git")).unwrap();
    write_pin(&root, "pinned-name");

    let _guard = EnvGuard::set("operator-choice");
    let got = resolve_palace(&root).expect("resolves");
    assert_eq!(got.id, "operator-choice");
    assert_eq!(got.source, PalaceSource::EnvOverride);
}

/// Why: an override that slugifies to nothing must not shadow the pin — it is
/// not a choice, it is an empty variable.
#[test]
#[serial_test::serial]
fn blank_env_override_falls_through_to_pin() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("proj");
    fs::create_dir_all(root.join(".git")).unwrap();
    write_pin(&root, "pinned-name");

    let _guard = EnvGuard::set("   ");
    let got = resolve_palace(&root).expect("resolves");
    assert_eq!(got.id, "pinned-name");
    assert_eq!(got.source, PalaceSource::PinFile);
}

/// Why (#2443): this level slugifies the variable itself rather than going
/// through the pure core, so it was the one derived id with no length bound —
/// an over-long `TRUSTY_MEMORY_PALACE` reached `palace_create` and was rejected,
/// leaving the caller's sink dead. Every level must produce an id the daemon's
/// gate accepts.
#[test]
#[serial_test::serial]
fn an_over_long_env_override_still_resolves_to_an_acceptable_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("proj");
    fs::create_dir_all(root.join(".git")).unwrap();

    let _guard = EnvGuard::set(&"Very_Long Operator Palace Name ".repeat(6));
    let got = resolve_palace(&root).expect("resolves");
    assert_eq!(got.source, PalaceSource::EnvOverride);
    assert!(
        crate::palace_id::palace_id_is_valid(&got.id),
        "override-derived id must pass the daemon gate, got {:?} ({} bytes)",
        got.id,
        got.id.len()
    );
}

/// Why (#2443): the `parent/dir` fallback is what a project with no remote and
/// no pin gets, and a long directory name pushed it past the daemon's limit.
#[test]
#[serial_test::serial]
fn a_long_project_dir_resolves_to_an_acceptable_id() {
    let _guard = EnvGuard::clear();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp
        .path()
        .join("a-project-directory-named-far-past-any-reasonable-length-limit-indeed");
    fs::create_dir_all(root.join(TRUSTY_TOOLS_DIR)).unwrap();

    let got = resolve_palace(&root).expect("resolves");
    assert!(
        crate::palace_id::palace_id_is_valid(&got.id),
        "parent/dir id must pass the daemon gate, got {:?} ({} bytes)",
        got.id,
        got.id.len()
    );
}

/// Why: a malformed pin must error even when the env override would have won
/// anyway. The pin is read before precedence is applied precisely so a broken
/// pin cannot hide behind a variable that happens to be set today and gone
/// tomorrow.
#[test]
#[serial_test::serial]
fn malformed_pin_errors_even_under_env_override() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("proj");
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join(TRUSTY_TOOLS_DIR)).unwrap();
    fs::write(root.join(PIN_FILE_REL), "palace: [unclosed\n").unwrap();

    let _guard = EnvGuard::set("operator-choice");
    let err = resolve_palace(&root).expect_err("must error");
    assert!(
        matches!(err, PalaceResolveError::PinMalformed { .. }),
        "expected PinMalformed, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Worktree stability — ADR-0012 §1
// ---------------------------------------------------------------------------

/// Why: a palace slug is per-project and shared across all worktrees and
/// branches of the same repo (ADR-0012 §1). Before this fix the `parent/dir`
/// fallback keyed on `git rev-parse --show-toplevel`, which names the
/// WORKTREE — so a worktree at
/// `<root>/.claude/worktrees/agent-x` derived `worktrees-agent-x` while its main
/// checkout derived `<parent>-<root>`. Two palaces, one project.
///
/// This exercises the unpinned, REMOTELESS case on purpose: with a pin or a
/// remote the two agree for unrelated reasons, so only this case proves the
/// `--git-common-dir` change.
#[test]
#[serial_test::serial]
fn worktree_and_main_checkout_agree() {
    let _guard = EnvGuard::clear();
    let tmp = tempfile::tempdir().expect("tempdir");
    let main = tmp.path().join("my-project");
    fs::create_dir_all(&main).unwrap();
    if !init_repo(&main, None) {
        eprintln!("skipping worktree_and_main_checkout_agree: git unavailable");
        return;
    }
    // A worktree needs at least one commit to branch from.
    fs::write(main.join("README.md"), "x").unwrap();
    let run = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(&main)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !run(&["add", "-A"]) || !run(&["commit", "-qm", "init"]) {
        eprintln!("skipping worktree_and_main_checkout_agree: cannot commit");
        return;
    }
    let wt = main.join(".claude").join("worktrees").join("agent-x");
    if !run(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "wt-branch",
        wt.to_str().unwrap(),
    ]) {
        eprintln!("skipping worktree_and_main_checkout_agree: cannot add worktree");
        return;
    }

    let from_main = resolve_palace(&main).expect("main resolves");
    let from_worktree = resolve_palace(&wt).expect("worktree resolves");

    assert_eq!(
        from_main.id, from_worktree.id,
        "a worktree must resolve to the same palace as its main checkout \
         (main={:?} from {:?}, worktree={:?} from {:?})",
        from_main.id, from_main.source, from_worktree.id, from_worktree.source
    );
    assert_eq!(from_main.source, PalaceSource::ParentDir);
    assert_eq!(from_worktree.source, PalaceSource::ParentDir);
}

/// Why: a pinned repo must also agree across worktrees. The pin is a tracked
/// file, so it is checked out into every worktree — this asserts the resolver
/// actually reads it there rather than stopping at some other root.
#[test]
#[serial_test::serial]
fn worktree_and_main_checkout_agree_when_pinned() {
    let _guard = EnvGuard::clear();
    let tmp = tempfile::tempdir().expect("tempdir");
    let main = tmp.path().join("my-project");
    fs::create_dir_all(&main).unwrap();
    if !init_repo(&main, Some("git@github.com:acme/widget.git")) {
        eprintln!("skipping worktree_and_main_checkout_agree_when_pinned: git unavailable");
        return;
    }
    write_pin(&main, "legacy-widget");
    let run = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(&main)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !run(&["add", "-A"]) || !run(&["commit", "-qm", "init"]) {
        eprintln!("skipping worktree_and_main_checkout_agree_when_pinned: cannot commit");
        return;
    }
    let wt = main.join(".claude").join("worktrees").join("agent-y");
    if !run(&["worktree", "add", "-q", "-b", "wt2", wt.to_str().unwrap()]) {
        eprintln!("skipping worktree_and_main_checkout_agree_when_pinned: cannot add worktree");
        return;
    }

    let from_main = resolve_palace(&main).expect("main resolves");
    let from_worktree = resolve_palace(&wt).expect("worktree resolves");
    assert_eq!(from_main.id, "legacy-widget");
    assert_eq!(from_worktree.id, "legacy-widget");
    assert_eq!(from_worktree.source, PalaceSource::PinFile);
}

// ---------------------------------------------------------------------------
// git helpers
// ---------------------------------------------------------------------------

/// Why (#5819): `--git-common-dir` is `<outer>/.git/modules/<name>` inside a
/// submodule, so "parent of the common dir" is `<outer>/.git/modules` — git
/// internals, not a working tree — and the probe returned it confidently. A
/// caller cannot tell that answer from a real root by looking at the path, and
/// trusty-memory's prompt-context filter drops every drawer whose recorded cwd
/// falls outside the root it is given.
/// What: builds the `.git`-file structure a submodule produces, via `git init
/// --separate-git-dir` into `<outer>/.git/modules/sub`, then asserts the probe
/// declines instead of naming the internals directory.
/// Test: itself. Fails against `0ac9e1f4`, which returns
/// `<outer>/.git/modules`.
#[test]
fn separate_git_dir_child_yields_none_not_a_git_internals_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let outer = tmp.path().join("outer");
    let sub = outer.join("sub");
    fs::create_dir_all(&sub).unwrap();
    if !init_repo(&outer, None) {
        eprintln!(
            "skipping separate_git_dir_child_yields_none_not_a_git_internals_path: git unavailable"
        );
        return;
    }
    let modules = outer.join(".git").join("modules");
    fs::create_dir_all(&modules).unwrap();
    let separate = modules.join("sub");
    let init = Command::new("git")
        .arg("-C")
        .arg(&sub)
        .args([
            "init",
            "-q",
            &format!("--separate-git-dir={}", separate.display()),
            ".",
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !init {
        eprintln!(
            "skipping separate_git_dir_child_yields_none_not_a_git_internals_path: cannot init"
        );
        return;
    }
    assert!(
        sub.join(".git").is_file(),
        "fixture must reproduce the `.git` FILE a submodule checkout carries"
    );

    let resolved = main_worktree_root(&sub);
    assert_eq!(
        resolved, None,
        "a common dir under `.git/modules` names no working tree; the probe must \
         decline rather than return {resolved:?}"
    );
}

/// Why: outside a repo both probes must decline rather than guess.
#[test]
fn git_probes_outside_a_repo_are_none() {
    let missing = Path::new("/nonexistent-trusty-test-root-5802");
    assert!(git_remote_origin(missing).is_none());
    assert!(main_worktree_root(missing).is_none());
}

/// Why: the explicit-remote channel exists for cloned sessions that know their
/// origin before a checkout exists; it must beat the probed remote.
#[test]
#[serial_test::serial]
fn explicit_remote_beats_probed_remote() {
    let _guard = EnvGuard::clear();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("proj");
    fs::create_dir_all(&root).unwrap();
    if !init_repo(&root, Some("git@github.com:probed/local.git")) {
        eprintln!("skipping explicit_remote_beats_probed_remote: git unavailable");
        return;
    }

    let got = resolve_palace_with_remote(&root, Some("git@github.com:acme/widget.git"))
        .expect("resolves");
    assert_eq!(got.id, "acme-widget");
    assert_eq!(got.source, PalaceSource::GitOwnerRepo);
}

// ---------------------------------------------------------------------------
// Env guard
// ---------------------------------------------------------------------------

/// Restores `TRUSTY_MEMORY_PALACE` to its prior value on drop.
///
/// Why: the variable is process-global; a test that leaked it would silently
/// pin every later test in the binary to one palace.
struct EnvGuard(Option<String>);

impl EnvGuard {
    fn clear() -> Self {
        let prior = std::env::var(crate::palace_id::PALACE_OVERRIDE_ENV).ok();
        // SAFETY: `#[serial]` serialises every test in this module that touches
        // this variable, so no other thread reads it concurrently.
        unsafe { std::env::remove_var(crate::palace_id::PALACE_OVERRIDE_ENV) };
        Self(prior)
    }

    fn set(value: &str) -> Self {
        let prior = std::env::var(crate::palace_id::PALACE_OVERRIDE_ENV).ok();
        // SAFETY: as above.
        unsafe { std::env::set_var(crate::palace_id::PALACE_OVERRIDE_ENV, value) };
        Self(prior)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: as above.
        unsafe {
            match self.0.take() {
                Some(v) => std::env::set_var(crate::palace_id::PALACE_OVERRIDE_ENV, v),
                None => std::env::remove_var(crate::palace_id::PALACE_OVERRIDE_ENV),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ProjectPin construction (#5811)
// ---------------------------------------------------------------------------

/// Why: [`ProjectPin`] is `#[non_exhaustive]`, so consumers cannot write the
/// struct literal and must go through [`ProjectPin::new`]. That constructor is
/// the only place the schema version is stamped, so a caller cannot pin an older
/// one by copying an old literal.
#[test]
fn new_stamps_the_current_schema_version() {
    let pin = ProjectPin::new("canonical-name");
    assert_eq!(pin.schema_version, PIN_SCHEMA_VERSION);
    assert_eq!(pin.palace, "canonical-name");
    assert_eq!(pin.note, None);
}

/// Why: `note` is `skip_serializing_if = "Option::is_none"`, so both the present
/// and absent shapes have to survive a YAML round trip through the reader every
/// consumer uses.
#[test]
fn with_note_round_trips_through_yaml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(tmp.path().join(TRUSTY_TOOLS_DIR)).unwrap();
    let pin = ProjectPin::new("canonical-name").with_note(Some("pinned before reorg".to_string()));
    fs::write(
        tmp.path().join(PIN_FILE_REL),
        serde_yaml::to_string(&pin).unwrap(),
    )
    .unwrap();

    let read_back = read_project_pin(tmp.path()).expect("ok").expect("some");
    assert_eq!(read_back, pin);
}

// ---------------------------------------------------------------------------
// Non-project callers stay resolvable
// ---------------------------------------------------------------------------

/// Why: not every palace belongs to a software project — trusty-agents mints one
/// per ASSISTANT, with no remote, no owner/repo and no project root. Making the
/// pin fail CLOSED (#5811) must not have made a git identity a REQUIREMENT: a
/// caller with none of those still resolves, via level 4, and only the three
/// pin-TRUST failures produce an error. `NoIdentity` is the separate, narrow
/// case where even the `parent/dir` slug is empty.
///
/// Creating an arbitrary palace by NAME never reaches this resolver at all —
/// `palace_create { force: true }` bypasses trusty-memory's `validate_palace_name`
/// gate outright (`dispatch_palace_create_force_allowed_in_single_tenant_default`),
/// which is the path trusty-agents' `TrustyMemoryClient::ensure_palace` uses.
#[test]
#[serial_test::serial]
fn a_caller_with_no_git_identity_still_resolves() {
    let _env = EnvGuard::clear();
    // No `.git`, no remote, no pin, no project marker of any kind.
    let tmp = tempfile::tempdir().expect("tempdir");
    let plain = tmp.path().join("assistant-scratch");
    fs::create_dir_all(&plain).unwrap();

    let resolved = resolve_palace(&plain).expect("a directory with no git identity must resolve");

    assert_eq!(resolved.source, PalaceSource::ParentDir);
    assert!(
        !resolved.id.is_empty(),
        "level 4 must still produce a usable id"
    );
}
