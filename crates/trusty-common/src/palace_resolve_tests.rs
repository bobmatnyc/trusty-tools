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

/// Build a repo at `<tmp>/my-project` with a linked worktree under
/// `.claude/worktrees/agent-x`, mirroring this repo's own layout.
///
/// Why: a hand-written `.git` file would prove the parser and nothing else. Only
/// a real `git worktree add` writes the admin directory — `commondir` included —
/// that [`main_checkout_of_worktree`] reads.
/// What: returns `(main, worktree)`, or `None` when git cannot do its part, so
/// the caller skips rather than failing on a machine without git.
fn repo_with_worktree(tmp: &Path) -> Option<(PathBuf, PathBuf)> {
    let main = tmp.join("my-project");
    fs::create_dir_all(&main).unwrap();
    if !init_repo(&main, None) {
        return None;
    }
    let run = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(&main)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    // A worktree needs at least one commit to branch from.
    fs::write(main.join("README.md"), "x").unwrap();
    if !run(&["add", "-A"]) || !run(&["commit", "-qm", "init"]) {
        return None;
    }
    let wt = main.join(".claude").join("worktrees").join("agent-x");
    if !run(&["worktree", "add", "-q", "-b", "wt-branch", wt.to_str()?]) {
        return None;
    }
    Some((main, wt))
}

/// Why (#5888): THE defect. `current.join(".git").exists()` is true for the
/// `.git` FILE git writes in a linked worktree, so the walk stopped at the
/// worktree and every caller read — and lazily WROTE — the pin there instead of
/// in the checkout the project lives in. A worktree and its main checkout must
/// answer the same root (ADR-0012 §1).
/// What: a real `git worktree add`, guarded by an `is_file` assertion on the
/// fixture, resolved from the worktree root and from a directory nested inside
/// it.
/// Test: itself. Against `4a823a0b4` both assertions return the worktree.
#[test]
fn a_worktree_resolves_to_its_main_checkout() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let Some((main, wt)) = repo_with_worktree(tmp.path()) else {
        eprintln!("skipping a_worktree_resolves_to_its_main_checkout: git unavailable");
        return;
    };
    assert!(
        wt.join(".git").is_file(),
        "fixture must reproduce git's linked-worktree `.git` FILE"
    );
    let expected = fs::canonicalize(&main).expect("canonicalize main");

    assert_eq!(
        find_project_root(&wt).expect("worktree resolves"),
        expected,
        "a worktree must resolve to the checkout that owns it"
    );

    let nested = wt.join("crates").join("foo");
    fs::create_dir_all(&nested).unwrap();
    assert_eq!(
        find_project_root(&nested).expect("nested dir resolves"),
        expected,
        "a directory inside a worktree must resolve to the same checkout"
    );
}

/// Why (#5888): a submodule and any `--separate-git-dir` checkout carry a `.git`
/// file of the identical shape, and their own directory IS the project root.
/// Following that pointer would hand the caller `<outer>/.git/modules` — git
/// internals, not a working tree, the same wrong answer #5819 removed from
/// [`main_worktree_root`].
/// What: builds the `--separate-git-dir` structure, asserts the fixture really
/// wrote a `.git` file, and asserts the walk still stops at the checkout.
#[test]
fn a_separate_git_dir_checkout_is_its_own_root() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let outer = tmp.path().join("outer");
    let sub = outer.join("sub");
    fs::create_dir_all(&sub).unwrap();
    if !init_repo(&outer, None) {
        eprintln!("skipping a_separate_git_dir_checkout_is_its_own_root: git unavailable");
        return;
    }
    let separate = outer.join(".git").join("modules").join("sub");
    fs::create_dir_all(separate.parent().unwrap()).unwrap();
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
        eprintln!("skipping a_separate_git_dir_checkout_is_its_own_root: cannot init");
        return;
    }
    assert!(
        sub.join(".git").is_file(),
        "fixture must reproduce the `.git` FILE a submodule checkout carries"
    );

    assert_eq!(
        find_project_root(&sub).expect("resolves"),
        fs::canonicalize(&sub).expect("canonicalize sub"),
        "a checkout whose git dir is elsewhere is still its own project root"
    );
}

/// Why (#5888): the redirect is best-effort. A `.git` file that does not parse,
/// or whose pointer names nothing, must leave the caller with the answer it had
/// before rather than with `None` — losing the root is worse than not following
/// the pointer.
#[test]
fn a_malformed_dot_git_file_leaves_the_directory_as_the_root() {
    let tmp = tempfile::tempdir().expect("tempdir");
    for (name, body) in [
        ("garbage", "not a gitdir pointer at all\n"),
        ("empty-pointer", "gitdir:   \n"),
        ("dangling", "gitdir: /nonexistent-trusty-test-admin-5888\n"),
    ] {
        let root = tmp.path().join(name);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".git"), body).unwrap();

        assert_eq!(
            find_project_root(&root).expect("resolves"),
            fs::canonicalize(&root).expect("canonicalize root"),
            "a `.git` file that does not resolve ({name}) must leave the directory as the root"
        );
    }
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

/// Why (#6418): the pin level used to return its `palace` field verbatim while
/// levels 1, 3 and 4 all clamped, so a dotted or traversal-shaped id could reach
/// a palace directory name and a socket filename. Each input here came back
/// unchanged from `resolve_palace` before the fix.
#[test]
#[serial_test::serial]
fn a_pin_naming_an_invalid_palace_id_is_an_error() {
    // The variable is cleared so the assertion is about level 2, not about the
    // override that outranks it.
    let _guard = EnvGuard::clear();
    let too_long = "a".repeat(PALACE_ID_MAX_LEN + 1);
    let cases = [
        "tripbot.tours",
        "../evil",
        "..",
        "a/b",
        "UPPER",
        "has_underscore",
        "-leading-hyphen",
        too_long.as_str(),
    ];
    for value in cases {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("proj");
        fs::create_dir_all(root.join(".git")).unwrap();
        write_pin(&root, value);

        match resolve_palace(&root) {
            Ok(resolution) => panic!("pin {value:?} resolved to {:?}", resolution.id),
            Err(PalaceResolveError::PinInvalid { value: got, .. }) => {
                assert_eq!(got, value, "the error must name the rejected value");
            }
            Err(other) => panic!("pin {value:?} expected PinInvalid, got {other:?}"),
        }
    }
}

/// Why (#6418): the guard rejects an id shape, not a legitimate pin — a pin that
/// already satisfies the daemon's format gate must still resolve, and surrounding
/// whitespace is not a shape violation.
#[test]
#[serial_test::serial]
fn a_valid_pin_still_resolves_and_is_trimmed() {
    let _guard = EnvGuard::clear();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("proj");
    fs::create_dir_all(root.join(".git")).unwrap();
    write_pin(&root, "  canonical-name9  ");

    let got = resolve_palace(&root).expect("a valid pin resolves");
    assert_eq!(got.id, "canonical-name9");
    assert_eq!(got.source, PalaceSource::PinFile);
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
// Pin schema version and unknown fields (#9274, ADR-0067 D2)
// ---------------------------------------------------------------------------

/// Why (#9274): a pin written by a newer release must be refused, not read as
/// if it were current. Before the fix the reader never looked at
/// `schema_version`, so both shapes below were either accepted (the first) or
/// misreported as malformed (the second, whose newer layout has no `palace`).
#[test]
fn a_newer_pin_schema_is_refused_naming_the_file_and_both_versions() {
    let newer_shapes = [
        "schema_version: 2\npalace: future-name\nproject_uuid: 0f3c\n",
        "schema_version: 2\npalaces:\n  - future-name\n",
    ];
    for raw in newer_shapes {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("proj");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join(TRUSTY_TOOLS_DIR)).unwrap();
        let pin_path = root.join(PIN_FILE_REL);
        fs::write(&pin_path, raw).unwrap();

        let err = read_project_pin(&root).expect_err("a newer pin must be refused");
        assert!(
            !matches!(err, PalaceResolveError::PinMalformed { .. }),
            "a newer pin is not a malformed one: {err:?}"
        );
        let msg = err.to_string();
        let path_text = pin_path.display().to_string();
        for needle in [path_text.as_str(), "schema_version 2", "schema_version 1"] {
            assert!(msg.contains(needle), "`{msg}` must name `{needle}`");
        }

        // The resolver must stop on it, not derive past it.
        // The resolver canonicalises the root, so compare everything but the path.
        let err = resolve_palace(&root).expect_err("resolver must not fall through");
        let resolver_msg = err.to_string();
        assert!(
            resolver_msg.contains("schema_version 2") && resolver_msg.contains(PIN_FILE_REL),
            "resolver must report the newer pin: {resolver_msg}"
        );
    }
}

/// Why (#9274): a field a later release adds to the pin must survive this
/// binary reading the pin and writing it back. Before the fix `ProjectPin` had
/// no catch-all, so serialising the parsed pin dropped both keys below.
#[test]
fn unknown_pin_fields_survive_a_read_and_rewrite() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(tmp.path().join(TRUSTY_TOOLS_DIR)).unwrap();
    fs::write(
        tmp.path().join(PIN_FILE_REL),
        "schema_version: 1\npalace: canonical-name\nproject_uuid: 0f3c\nsync:\n  enabled: true\n",
    )
    .unwrap();

    let pin = read_project_pin(tmp.path()).expect("ok").expect("some");
    assert_eq!(pin.palace, "canonical-name");
    let rewritten: serde_yaml::Value =
        serde_yaml::from_str(&serde_yaml::to_string(&pin).expect("serialise")).expect("parse");
    assert_eq!(rewritten["project_uuid"].as_str(), Some("0f3c"));
    assert_eq!(rewritten["sync"]["enabled"].as_bool(), Some(true));
    assert_eq!(rewritten["palace"].as_str(), Some("canonical-name"));
}

/// Why (#9274): a freshly built pin replacing one on disk takes the old pin's
/// unknown keys, but never its known fields — the new `palace` must win.
#[test]
fn preserving_unknown_fields_of_copies_only_the_unknown_keys() {
    let on_disk: ProjectPin = serde_yaml::from_str(
        "schema_version: 1\npalace: old-name\nnote: old note\nproject_uuid: 0f3c\n",
    )
    .expect("parse");

    let pin = ProjectPin::new("new-name").preserving_unknown_fields_of(&on_disk);

    assert_eq!(pin.palace, "new-name");
    assert_eq!(pin.note, None);
    let written: serde_yaml::Value =
        serde_yaml::from_str(&serde_yaml::to_string(&pin).expect("serialise")).expect("parse");
    assert_eq!(written["project_uuid"].as_str(), Some("0f3c"));
    assert!(written.get("note").is_none(), "a known field is not copied");
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

// ---------------------------------------------------------------------------
// plan_palace_create (#11823)
// ---------------------------------------------------------------------------

/// Plan for `start` against a fixed set of existing palace ids, recording
/// every id the oracle was asked about.
fn plan_against(
    start: &Path,
    existing: &[&str],
) -> (Result<PalaceCreatePlan, PalaceCreatePlanError>, Vec<String>) {
    let asked = std::cell::RefCell::new(Vec::new());
    let plan = plan_palace_create(start, |id| {
        asked.borrow_mut().push(id.to_string());
        Ok::<_, std::convert::Infallible>(existing.contains(&id))
    });
    (plan, asked.into_inner())
}

/// Build `<tmp>/<dir>` as a git repo with an optional origin remote, or `None`
/// when git is unavailable.
fn plan_repo(tmp: &Path, dir: &str, remote: Option<&str>) -> Option<PathBuf> {
    let root = tmp.join(dir);
    fs::create_dir_all(&root).unwrap();
    init_repo(&root, remote).then_some(root)
}

/// Why: a repo with no remote resolves to `parent-dir`, but older creators
/// named its palace after the folder alone; the launch-time alias step never
/// covered this shape (#11823).
#[test]
#[serial_test::serial]
fn plan_aliases_for_a_repo_with_no_remote() {
    let _guard = EnvGuard::clear();
    let tmp = tempfile::tempdir().expect("tempdir");
    let Some(root) = plan_repo(&tmp.path().join("projects"), "my-repo", None) else {
        eprintln!("skipping plan_aliases_for_a_repo_with_no_remote: git unavailable");
        return;
    };
    let (plan, _) = plan_against(&root, &["my-repo"]);
    assert_eq!(
        plan.expect("plans"),
        PalaceCreatePlan::AliasToFolder {
            resolved: "projects-my-repo".into(),
            folder: "my-repo".into(),
        }
    );
}

/// Why: `Acme/My_Repo` checked out as `checkout-dir` resolves to
/// `acme-my-repo`; the palace an older creator made is `checkout-dir`, and it
/// must be linked, not hidden behind a new empty palace (#11823).
#[test]
#[serial_test::serial]
fn plan_aliases_when_folder_differs_from_repo_name() {
    let _guard = EnvGuard::clear();
    let tmp = tempfile::tempdir().expect("tempdir");
    let remote = Some("git@github.com:Acme/My_Repo.git");
    let Some(root) = plan_repo(tmp.path(), "checkout-dir", remote) else {
        eprintln!("skipping plan_aliases_when_folder_differs_from_repo_name: git unavailable");
        return;
    };
    let (plan, asked) = plan_against(&root, &["checkout-dir"]);
    assert_eq!(
        plan.expect("plans"),
        PalaceCreatePlan::AliasToFolder {
            resolved: "acme-my-repo".into(),
            folder: "checkout-dir".into(),
        }
    );
    assert_eq!(asked, ["acme-my-repo", "checkout-dir"]);
}

/// Why: an existing resolved palace needs nothing, even when a folder palace
/// also exists — the folder palace is left alone.
#[test]
#[serial_test::serial]
fn plan_exists_when_the_resolved_palace_exists() {
    let _guard = EnvGuard::clear();
    let tmp = tempfile::tempdir().expect("tempdir");
    let remote = Some("git@github.com:Acme/My_Repo.git");
    let Some(root) = plan_repo(tmp.path(), "checkout-dir", remote) else {
        eprintln!("skipping plan_exists_when_the_resolved_palace_exists: git unavailable");
        return;
    };
    let (plan, asked) = plan_against(&root, &["acme-my-repo", "checkout-dir"]);
    assert_eq!(
        plan.expect("plans"),
        PalaceCreatePlan::Exists {
            id: "acme-my-repo".into()
        }
    );
    assert_eq!(
        asked,
        ["acme-my-repo"],
        "the folder palace is never consulted"
    );
}

/// Why: a new project with no palace under either name gets the resolved one.
#[test]
#[serial_test::serial]
fn plan_creates_when_neither_palace_exists() {
    let _guard = EnvGuard::clear();
    let tmp = tempfile::tempdir().expect("tempdir");
    let remote = Some("git@github.com:Acme/My_Repo.git");
    let Some(root) = plan_repo(tmp.path(), "checkout-dir", remote) else {
        eprintln!("skipping plan_creates_when_neither_palace_exists: git unavailable");
        return;
    };
    let (plan, _) = plan_against(&root, &["unrelated"]);
    assert_eq!(
        plan.expect("plans"),
        PalaceCreatePlan::Create {
            id: "acme-my-repo".into()
        }
    );
}

/// Why: when the folder slug equals the resolved id there is no second palace
/// to link; the plan must not alias a name to itself.
#[test]
#[serial_test::serial]
fn plan_never_aliases_a_palace_to_itself() {
    let _guard = EnvGuard::clear();
    let tmp = tempfile::tempdir().expect("tempdir");
    let remote = Some("git@github.com:Acme/My_Repo.git");
    let Some(root) = plan_repo(tmp.path(), "acme-my-repo", remote) else {
        eprintln!("skipping plan_never_aliases_a_palace_to_itself: git unavailable");
        return;
    };
    let (plan, asked) = plan_against(&root, &[]);
    assert_eq!(
        plan.expect("plans"),
        PalaceCreatePlan::Create {
            id: "acme-my-repo".into()
        }
    );
    assert_eq!(asked, ["acme-my-repo"]);
}

/// Why: a malformed pin blocks the create and is reported; no fallback name is
/// planned and the daemon is never asked (#11823).
#[test]
#[serial_test::serial]
fn plan_propagates_a_malformed_pin() {
    let _guard = EnvGuard::clear();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("some-project");
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join(TRUSTY_TOOLS_DIR)).unwrap();
    fs::write(
        root.join(PIN_FILE_REL),
        "schema_version: 1\npalace:\n  not: a-string\n",
    )
    .unwrap();

    let (plan, asked) = plan_against(&root, &["some-project"]);
    let err = plan.expect_err("a malformed pin must not plan");
    assert!(
        matches!(
            err,
            PalaceCreatePlanError::Resolve(PalaceResolveError::PinMalformed { .. })
        ),
        "expected Resolve(PinMalformed), got {err:?}"
    );
    assert!(
        asked.is_empty(),
        "the oracle must not be consulted: {asked:?}"
    );
}

/// Why: Q5 of the #11823 ruling — a create with no project directory is
/// rejected, even though the resolver can still derive a name for it.
#[test]
#[serial_test::serial]
fn plan_rejects_a_start_with_no_project_dir() {
    let _guard = EnvGuard::clear();
    let missing = Path::new("/nonexistent-trusty-test-root-11823/deeper");
    let (plan, asked) = plan_against(missing, &[]);
    let err = plan.expect_err("no project dir must not plan");
    assert!(
        matches!(err, PalaceCreatePlanError::NoProjectDir { .. }),
        "expected NoProjectDir, got {err:?}"
    );
    assert!(
        asked.is_empty(),
        "the oracle must not be consulted: {asked:?}"
    );
}

/// Why: an env override or a committed pin is the user's explicit choice, so
/// a missing chosen palace is created, never redirected to the folder palace.
#[test]
#[serial_test::serial]
fn plan_never_aliases_an_env_or_pin_choice() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let remote = Some("git@github.com:Acme/My_Repo.git");
    let Some(root) = plan_repo(tmp.path(), "checkout-dir", remote) else {
        eprintln!("skipping plan_never_aliases_an_env_or_pin_choice: git unavailable");
        return;
    };

    {
        let _guard = EnvGuard::set("operator-choice");
        let (plan, asked) = plan_against(&root, &["checkout-dir"]);
        assert_eq!(
            plan.expect("plans"),
            PalaceCreatePlan::Create {
                id: "operator-choice".into()
            }
        );
        assert_eq!(asked, ["operator-choice"]);
        let (plan, _) = plan_against(&root, &["operator-choice"]);
        assert_eq!(
            plan.expect("plans"),
            PalaceCreatePlan::Exists {
                id: "operator-choice".into()
            }
        );
    }

    let _guard = EnvGuard::clear();
    write_pin(&root, "pinned-name");
    let (plan, asked) = plan_against(&root, &["checkout-dir"]);
    assert_eq!(
        plan.expect("plans"),
        PalaceCreatePlan::Create {
            id: "pinned-name".into()
        }
    );
    assert_eq!(asked, ["pinned-name"]);
}

/// Why: a worktree and its main checkout share one palace (ADR-0012 §1), so
/// they must plan the same action — including the same folder alias target.
#[test]
#[serial_test::serial]
fn plan_is_the_same_from_a_worktree_and_its_main_checkout() {
    let _guard = EnvGuard::clear();
    let tmp = tempfile::tempdir().expect("tempdir");
    let Some((main, wt)) = repo_with_worktree(tmp.path()) else {
        eprintln!(
            "skipping plan_is_the_same_from_a_worktree_and_its_main_checkout: git unavailable"
        );
        return;
    };
    let (from_main, _) = plan_against(&main, &["my-project"]);
    let (from_wt, _) = plan_against(&wt, &["my-project"]);
    let from_main = from_main.expect("main plans");
    assert_eq!(from_main, from_wt.expect("worktree plans"));
    assert!(
        matches!(
            &from_main,
            PalaceCreatePlan::AliasToFolder { folder, .. } if folder == "my-project"
        ),
        "expected an alias to the main checkout's folder palace, got {from_main:?}"
    );
}

/// Why (#11823, fail-open check): a lookup that cannot answer must refuse the
/// plan. Read as "absent", it would plan a `Create` over an existing palace or
/// skip the folder alias.
/// What: fails the lookup first for the resolved id, then only for the folder
/// id; both refuse with `Lookup` naming the id.
#[test]
#[serial_test::serial]
fn plan_refuses_when_a_lookup_fails() {
    let _guard = EnvGuard::clear();
    let tmp = tempfile::tempdir().expect("tempdir");
    let remote = Some("git@github.com:Acme/My_Repo.git");
    let Some(root) = plan_repo(tmp.path(), "checkout-dir", remote) else {
        eprintln!("skipping plan_refuses_when_a_lookup_fails: git unavailable");
        return;
    };
    for failing in ["acme-my-repo", "checkout-dir"] {
        let err = plan_palace_create(&root, |id| {
            if id == failing {
                Err(std::io::Error::other("daemon unreachable"))
            } else {
                Ok(false)
            }
        })
        .expect_err("a failed lookup must not plan");
        match err {
            PalaceCreatePlanError::Lookup { id, .. } => assert_eq!(id, failing),
            other => panic!("expected Lookup for {failing}, got {other:?}"),
        }
    }
}

/// Why (#11823): the daemon's own `TRUSTY_MEMORY_PALACE` must not name a
/// client's project palace.
/// What: with the variable set, the project form resolves and plans the git
/// `owner/repo` name, while the honouring form still returns the variable.
#[test]
#[serial_test::serial]
fn project_plan_ignores_the_process_palace_env() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let remote = Some("git@github.com:Acme/My_Repo.git");
    let Some(root) = plan_repo(tmp.path(), "checkout-dir", remote) else {
        eprintln!("skipping project_plan_ignores_the_process_palace_env: git unavailable");
        return;
    };
    let _guard = EnvGuard::set("daemon-palace");
    let resolved = resolve_palace_for_project(&root).expect("resolves");
    assert_eq!(resolved.id, "acme-my-repo");
    assert_eq!(resolved.source, PalaceSource::GitOwnerRepo);
    let plan = plan_palace_create_for_project(&root, |id| {
        Ok::<_, std::convert::Infallible>(id == "checkout-dir")
    })
    .expect("plans");
    assert_eq!(
        plan,
        PalaceCreatePlan::AliasToFolder {
            resolved: "acme-my-repo".into(),
            folder: "checkout-dir".into(),
        }
    );
    assert_eq!(resolve_palace(&root).expect("resolves").id, "daemon-palace");
}

/// Why: the folder slug is the one rule both the plan and trusty-memory's
/// `project_slug_from_basename` use.
#[test]
fn folder_palace_slug_clamps_and_slugifies() {
    assert_eq!(
        folder_palace_slug(Path::new("/x/My_Repo")).as_deref(),
        Some("my-repo")
    );
    assert_eq!(folder_palace_slug(Path::new("/")), None);
    let long = folder_palace_slug(&Path::new("/x").join("a".repeat(80))).expect("slug");
    assert!(palace_id_is_valid(&long), "{long}");
}
