//! Tests for the ancestor memory-file scan and the launch WARN (#7673).
//!
//! Split out with `#[path]` so `ancestor_claude_md.rs` stays under the 500-SLOC
//! production cap.

use super::*;
use crate::core::instruction_pipeline::CLAUDE_MD_STUB;
use tempfile::TempDir;

/// `<tmp>/ancestor/project`, canonicalised so assertions compare the same
/// spelling the scan reports.
fn fixture() -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let ancestor = root.join("ancestor");
    let project = ancestor.join("project");
    std::fs::create_dir_all(&project).unwrap();
    (tmp, ancestor, project)
}

fn none() -> BTreeSet<String> {
    BTreeSet::new()
}

#[test]
fn the_token_estimate_divides_bytes_by_four() {
    let file = AncestorMemoryFile {
        path: PathBuf::from("/x/CLAUDE.md"),
        bytes: 4001,
        seed_template: false,
        excluded: false,
    };
    assert_eq!(BYTES_PER_TOKEN, 4);
    assert_eq!(file.token_estimate(), 1000);
}

#[test]
fn the_summary_names_the_divisor() {
    let file = AncestorMemoryFile {
        path: PathBuf::from("/x/CLAUDE.md"),
        bytes: 400,
        seed_template: true,
        excluded: false,
    };
    let line = file.summary();
    assert!(line.contains("/x/CLAUDE.md"), "{line}");
    assert!(line.contains("400 B"), "{line}");
    assert!(line.contains("~100 tokens at bytes/4"), "{line}");
    assert!(line.contains("tm seed template"), "{line}");
}

#[test]
fn no_ancestors_yields_nothing() {
    let (_tmp, _ancestor, project) = fixture();
    assert!(
        scan_with_excludes(&project, &none())
            .unwrap()
            .found
            .is_empty()
    );
}

/// FAILS BEFORE THIS CHANGE: nothing walked above the project root, so the
/// `$HOME` seed template was invisible to every surface in the harness.
#[test]
fn a_seed_ancestor_is_reported_as_a_seed() {
    let (_tmp, ancestor, project) = fixture();
    let file = ancestor.join("CLAUDE.md");
    std::fs::write(&file, CLAUDE_MD_STUB).unwrap();

    let found = scan_with_excludes(&project, &none()).unwrap().found;

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, file);
    assert!(found[0].seed_template);
    assert!(!found[0].excluded);
    assert_eq!(found[0].bytes, CLAUDE_MD_STUB.len() as u64);
}

#[test]
fn a_content_ancestor_is_not_a_seed() {
    let (_tmp, ancestor, project) = fixture();
    std::fs::write(ancestor.join("CLAUDE.md"), "# Monorepo\n\nUse pnpm.\n").unwrap();

    let found = scan_with_excludes(&project, &none()).unwrap().found;

    assert_eq!(found.len(), 1);
    assert!(!found[0].seed_template);
}

/// The project's OWN instructions are not an ancestor and must never appear —
/// the repair would otherwise rename or exclude the file the session needs.
#[test]
fn a_project_root_file_is_never_reported() {
    let (_tmp, _ancestor, project) = fixture();
    std::fs::write(project.join("CLAUDE.md"), CLAUDE_MD_STUB).unwrap();

    assert!(
        scan_with_excludes(&project, &none())
            .unwrap()
            .found
            .is_empty()
    );
}

#[test]
fn every_memory_file_shape_is_reported() {
    let (_tmp, ancestor, project) = fixture();
    std::fs::create_dir_all(ancestor.join(".claude")).unwrap();
    for relative in ["CLAUDE.md", "CLAUDE.local.md", ".claude/CLAUDE.md"] {
        std::fs::write(ancestor.join(relative), "# notes\n").unwrap();
    }

    let found = scan_with_excludes(&project, &none()).unwrap().found;

    assert_eq!(found.len(), 3, "{found:?}");
}

#[test]
fn an_excluded_ancestor_is_flagged_excluded() {
    let (_tmp, ancestor, project) = fixture();
    let file = ancestor.join("CLAUDE.md");
    std::fs::write(&file, "# notes\n").unwrap();
    let excludes: BTreeSet<String> = [file.display().to_string()].into_iter().collect();

    let found = scan_with_excludes(&project, &excludes).unwrap().found;

    assert_eq!(found.len(), 1);
    assert!(found[0].excluded);
}

#[test]
fn a_large_ancestor_is_not_opened_or_called_a_seed() {
    let (_tmp, ancestor, project) = fixture();
    let big = "x".repeat((MAX_SEED_BYTES + 1) as usize);
    std::fs::write(ancestor.join("CLAUDE.md"), &big).unwrap();

    let found = scan_with_excludes(&project, &none()).unwrap().found;

    assert_eq!(found.len(), 1);
    assert!(!found[0].seed_template);
}

#[test]
fn warn_is_silent_when_there_are_no_ancestors() {
    assert_eq!(warning_text(&[]), None);
}

#[test]
fn an_excluded_ancestor_produces_no_warning() {
    let found = vec![AncestorMemoryFile {
        path: PathBuf::from("/x/CLAUDE.md"),
        bytes: 400,
        seed_template: false,
        excluded: true,
    }];
    assert_eq!(warning_text(&found), None);
}

#[test]
fn the_warning_names_both_remedies() {
    let found = vec![AncestorMemoryFile {
        path: PathBuf::from("/Users/ada/CLAUDE.md"),
        bytes: 1200,
        seed_template: true,
        excluded: false,
    }];

    let text = warning_text(&found).expect("a loading ancestor warns");

    assert!(text.contains("/Users/ada/CLAUDE.md"), "{text}");
    assert!(text.contains("~300 tokens"), "{text}");
    assert!(text.contains("delete the file"), "{text}");
    assert!(text.contains("claudeMdExcludes"), "{text}");
}

#[test]
fn the_rename_target_carries_the_date() {
    assert_eq!(
        stale_seed_name(Path::new("/Users/ada/CLAUDE.md"), "20260912"),
        PathBuf::from("/Users/ada/CLAUDE.md.stale-seed-20260912")
    );
}

/// `git -C <dir> init -q`, reporting whether git was available at all.
fn git_init(dir: &Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["init", "-q"])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Regression case 2 (#7673 round 2): a scan started from a SUBDIRECTORY of a
/// git repository — `tm doctor` run from `crates/trusty-mpm` in this repo —
/// reported the repository's own root `CLAUDE.md` as a stray ancestor. The
/// nearest boundary is the repository root, and nothing at or below it is
/// reported. Round 3 already passed this; it pins that the nearest-wins rule
/// does not reopen it.
#[test]
fn scan_does_not_report_the_git_roots_own_claude_md_for_a_nested_project_root() {
    let tmp = TempDir::new().unwrap();
    let repo = std::fs::canonicalize(tmp.path()).unwrap().join("repo");
    let nested = repo.join("crates").join("thing");
    std::fs::create_dir_all(&nested).unwrap();
    if !git_init(&repo) {
        eprintln!("#7673 tests: git unavailable, skipping");
        return;
    }
    std::fs::write(repo.join("CLAUDE.md"), "# Root project\n\nUse cargo.\n").unwrap();

    let found = scan(&nested, None, None).unwrap().found;

    assert!(found.is_empty(), "{found:?}");
}

/// The registered-project half of [`resolve_project_root`]: a non-git project
/// marked by `.trusty-mpm` is found from a nested subdirectory too.
#[test]
fn resolve_project_root_finds_the_registered_marker_above_a_nested_dir() {
    let tmp = TempDir::new().unwrap();
    let project = std::fs::canonicalize(tmp.path())
        .unwrap()
        .join("registered");
    std::fs::create_dir_all(project.join(".trusty-mpm")).unwrap();
    let nested = project.join("sub").join("dir");
    std::fs::create_dir_all(&nested).unwrap();

    assert_eq!(resolve_project_root(&nested, None).unwrap(), project);
}

/// With neither a git ancestor nor a `.trusty-mpm` marker, `resolve_project_root`
/// falls back to the given directory itself, canonicalized.
#[test]
fn resolve_project_root_falls_back_to_the_given_dir_with_no_git_or_marker() {
    let tmp = TempDir::new().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap().join("scratch");
    std::fs::create_dir_all(&dir).unwrap();

    assert_eq!(resolve_project_root(&dir, None).unwrap(), dir);
}

/// `scan` (as opposed to `scan_with_excludes`) resolves the exclude set from the
/// project's own settings layers.
#[test]
fn scan_reads_the_excludes_from_the_project_layer() {
    let (_tmp, ancestor, project) = fixture();
    let file = ancestor.join("CLAUDE.md");
    std::fs::write(&file, "# notes\n").unwrap();
    let settings = project.join(".claude").join("settings.local.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        serde_json::json!({ "claudeMdExcludes": [file.display().to_string()] }).to_string(),
    )
    .unwrap();

    let found = scan(&project, None, None).unwrap().found;

    assert_eq!(found.len(), 1);
    assert!(found[0].excluded, "{found:?}");
}

/// A fake `$HOME` that is a git repository (a dotfiles repo) with real content
/// in its root `CLAUDE.md`, holding `projects/myproject/.trusty-mpm/` with no
/// git of its own. Returns `(tmp, home, myproject, home CLAUDE.md)`, or `None`
/// when git is unavailable.
fn dotfiles_home() -> Option<(TempDir, PathBuf, PathBuf, PathBuf)> {
    let tmp = TempDir::new().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap().join("home");
    let project = home.join("projects").join("myproject");
    std::fs::create_dir_all(project.join(".trusty-mpm")).unwrap();
    if !git_init(&home) {
        eprintln!("#7673 tests: git unavailable, skipping");
        return None;
    }
    let file = home.join("CLAUDE.md");
    std::fs::write(&file, "# Dotfiles\n\nMy shell notes.\n").unwrap();
    Some((tmp, home, project, file))
}

/// Regression case 1 — FAILS AGAINST ROUND 3 (#7673 round-3 CRITICAL): the
/// enclosing git toplevel won over the nearer `.trusty-mpm` marker, the project
/// resolved to the fake `$HOME`, and `$HOME/CLAUDE.md` went unreported — the
/// 2026-09-12 incident shape.
#[test]
fn a_marker_project_under_a_dotfiles_home_reports_the_home_claude_md() {
    let Some((_tmp, _home, project, file)) = dotfiles_home() else {
        return;
    };

    assert_eq!(resolve_project_root(&project, None).unwrap(), project);
    let found = scan(&project, None, None).unwrap().found;

    let paths: Vec<&PathBuf> = found.iter().map(|f| &f.path).collect();
    assert_eq!(paths, vec![&file], "{found:?}");
}

/// Regression case 3 — FAILS AGAINST ROUND 3: a marker project inside git repo
/// `inner`, inside git repo `outer`, each repo with a root `CLAUDE.md`. The
/// marker is the nearest boundary, so BOTH repo files sit above the root and
/// both are reported; nothing at or below the marker root is (the lower-level
/// invariant is `a_project_root_file_is_never_reported`).
#[test]
fn a_marker_project_inside_two_repos_reports_both_repo_roots() {
    let tmp = TempDir::new().unwrap();
    let outer = std::fs::canonicalize(tmp.path()).unwrap().join("outer");
    let inner = outer.join("inner");
    let project = inner.join("marker_project");
    std::fs::create_dir_all(project.join(".trusty-mpm")).unwrap();
    std::fs::create_dir_all(project.join("sub")).unwrap();
    if !git_init(&outer) || !git_init(&inner) {
        eprintln!("#7673 tests: git unavailable, skipping");
        return;
    }
    let outer_file = outer.join("CLAUDE.md");
    let inner_file = inner.join("CLAUDE.md");
    std::fs::write(&outer_file, "# Outer\n\nOuter notes.\n").unwrap();
    std::fs::write(&inner_file, "# Inner\n\nInner notes.\n").unwrap();
    std::fs::write(project.join("CLAUDE.md"), "# Mine\n").unwrap();
    std::fs::write(project.join("sub").join("CLAUDE.md"), "# Below\n").unwrap();

    let found = scan(&project.join("sub"), None, None).unwrap().found;

    let paths: BTreeSet<&PathBuf> = found.iter().map(|f| &f.path).collect();
    assert_eq!(
        paths,
        [&inner_file, &outer_file].into_iter().collect(),
        "{found:?}"
    );
    assert!(
        found.iter().all(|f| !f.path.starts_with(&project)),
        "nothing at or below the marker root is reported: {found:?}"
    );
}

/// Regression case 4 — FAILS AGAINST ROUND 3: a symlinked start directory
/// resolves exactly like its target, including the dotfiles-`$HOME` verdict.
#[cfg(unix)]
#[test]
fn a_symlinked_start_resolves_like_its_target() {
    let Some((tmp, _home, project, file)) = dotfiles_home() else {
        return;
    };
    let link = std::fs::canonicalize(tmp.path()).unwrap().join("link");
    std::os::unix::fs::symlink(&project, &link).unwrap();

    assert_eq!(
        resolve_project_root(&link, None).unwrap(),
        resolve_project_root(&project, None).unwrap()
    );
    let found = scan(&link, None, None).unwrap().found;
    assert_eq!(
        found.iter().map(|f| &f.path).collect::<Vec<_>>(),
        vec![&file],
        "{found:?}"
    );
}

/// Regression case 5 — FAILS AGAINST ROUND 3: a start directory that does not
/// exist resolved to itself and was scanned as if it existed; with no file
/// above it the result was an empty list, which every caller read as "no stray
/// files". It is an error, whatever sits above the missing path.
#[test]
fn a_missing_start_directory_is_an_error_not_an_empty_scan() {
    let (_tmp, ancestor, _project) = fixture();
    std::fs::write(ancestor.join("CLAUDE.md"), CLAUDE_MD_STUB).unwrap();
    let missing = ancestor.join("does-not-exist");

    let err = resolve_project_root(&missing, None).unwrap_err();
    assert!(err.to_string().contains("does-not-exist"), "{err}");
    assert!(scan(&missing, None, None).is_err());
}

/// FAILS AGAINST 89f6204e4 (#7673 critic MEDIUM): one permission-denied
/// ancestor made the whole scan an `Err`, on every launch and doctor run. It is
/// skipped and RECORDED, and the readable stray beside it is still reported.
/// Also fails if the denied directory were treated as clean with no record.
/// Skipped when running as root, where permission bits do not bind.
#[cfg(unix)]
#[test]
fn an_unreadable_ancestor_is_recorded_and_the_rest_still_reported() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, ancestor, project) = fixture();
    let readable = ancestor.join("CLAUDE.md");
    std::fs::write(&readable, "# notes\n").unwrap();
    let locked = ancestor.join(".claude");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("CLAUDE.md"), "# hidden\n").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let denied = std::fs::metadata(locked.join("CLAUDE.md")).is_err();

    let scanned = scan_with_excludes(&project, &none());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

    if !denied {
        eprintln!("#7673 tests: permission bits do not bind here, skipping");
        return;
    }
    let scanned = scanned.expect("a denied ancestor degrades the scan, it does not fail it");
    assert_eq!(
        scanned.found.iter().map(|f| &f.path).collect::<Vec<_>>(),
        vec![&readable],
        "{scanned:?}"
    );
    assert_eq!(scanned.unchecked, vec![locked], "{scanned:?}");
}

/// Only a denied permission degrades the scan: an ancestor probe that fails for
/// any other reason — here `ELOOP` from a self-referencing symlink — is still
/// an error, never a skip.
#[cfg(unix)]
#[test]
fn an_ancestor_stat_failing_for_another_reason_is_still_an_error() {
    let (_tmp, ancestor, project) = fixture();
    let looped = ancestor.join("CLAUDE.md");
    std::os::unix::fs::symlink(&looped, &looped).unwrap();

    let err = scan_with_excludes(&project, &none()).unwrap_err();

    assert_ne!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
    assert!(err.to_string().contains("CLAUDE.md"), "{err}");
}

/// A partial scan adds ONE sentence to the launch notice naming every skipped
/// directory — a WARN line, not the scan-error notice.
#[test]
fn a_partial_scan_notice_names_the_unchecked_directories_once() {
    let scanned = AncestorScan {
        found: Vec::new(),
        unchecked: vec![PathBuf::from("/mnt/a"), PathBuf::from("/mnt/b")],
    };

    let text = scan_notice(Path::new("/x/project"), &Ok(scanned)).expect("a skip is announced");

    assert!(text.contains("/mnt/a, /mnt/b"), "{text}");
    assert!(text.contains("not checked"), "{text}");
    assert_eq!(text.matches("could not be read").count(), 1, "{text}");
    assert!(!text.contains('\n'), "{text}");
    assert!(
        !text.contains("could not check"),
        "not the error notice: {text}"
    );
}

/// FAILS AGAINST ROUND 3: `$HOME` is never a project boundary. `~/.trusty-mpm`
/// is tm's global state directory and a dotfiles `.git` covers every directory
/// under `$HOME`; counting either resolved a project-less directory to `$HOME`
/// and hid `$HOME/CLAUDE.md`.
#[test]
fn the_home_directory_is_never_a_project_boundary() {
    let Some((_tmp, home, _project, file)) = dotfiles_home() else {
        return;
    };
    std::fs::create_dir_all(home.join(".trusty-mpm")).unwrap();
    let scratch = home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();

    assert_eq!(
        resolve_project_root(&scratch, Some(&home)).unwrap(),
        scratch
    );
    let found = scan(&scratch, Some(&home), None).unwrap().found;
    assert_eq!(
        found.iter().map(|f| &f.path).collect::<Vec<_>>(),
        vec![&file],
        "{found:?}"
    );
}

#[test]
fn a_failed_scan_is_announced_not_silent() {
    let err = io::Error::new(io::ErrorKind::NotFound, "gone");
    let text = scan_notice(Path::new("/x/project"), &Err(err)).expect("an error is announced");
    assert!(text.contains("/x/project"), "{text}");
    assert!(text.contains("gone"), "{text}");
    assert!(text.contains("NOT checked"), "{text}");
    assert_eq!(
        scan_notice(Path::new("/x"), &Ok(AncestorScan::default())),
        None
    );
}

/// Run `git` in `dir`, asserting success — a silently failed fixture step would
/// make every assertion after it meaningless.
fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} in {} failed to spawn: {e}", dir.display()));
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A committed repository at `dir` holding one file, `name`, with `body`.
fn committed_repo(dir: &Path, name: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@example.com"]);
    git(dir, &["config", "user.name", "T"]);
    std::fs::write(dir.join(name), body).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-qm", "init"]);
}

/// `<tmp>/repo` with a root `CLAUDE.md`, and the nested path Claude Code's own
/// worktree isolation uses. Returns `(tmp, repo, <repo>/.claude/worktrees/wt)`;
/// nothing exists at the nested path yet.
fn repo_with_nested_path() -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let repo = std::fs::canonicalize(tmp.path()).unwrap().join("repo");
    committed_repo(&repo, "CLAUDE.md", "# Repo\n\nUse cargo.\n");
    let nested = repo.join(".claude").join("worktrees").join("wt");
    std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
    (tmp, repo, nested)
}

/// FAILS AGAINST 89f6204e4 (#7673): a linked worktree nested inside its main
/// checkout stopped at the worktree's `.git` FILE and reported the checkout's
/// own `CLAUDE.md` as a stray ancestor. Measured: Claude Code gives such a
/// worktree the main checkout's project identity and does not pay for that
/// file twice, so the root is the main checkout and nothing is reported.
#[test]
fn a_linked_worktree_nested_in_its_checkout_reports_nothing() {
    let (_tmp, repo, wt) = repo_with_nested_path();
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
    );

    assert_eq!(resolve_project_root(&wt, None).unwrap(), repo);
    let found = scan(&wt, None, None).unwrap().found;
    assert!(found.is_empty(), "{found:?}");
}

/// An independent clone at the same nested path is a SEPARATE project: its own
/// `.git` directory is the boundary, and the enclosing repo's `CLAUDE.md` is a
/// real ancestor that loads into it. Guards Fix 1 against over-reaching.
#[test]
fn a_clone_nested_in_another_checkout_reports_the_outer_claude_md() {
    let (_tmp, repo, clone) = repo_with_nested_path();
    git(
        &repo,
        &[
            "clone",
            "-q",
            repo.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );

    assert_eq!(resolve_project_root(&clone, None).unwrap(), clone);
    let found = scan(&clone, None, None).unwrap().found;
    assert_eq!(
        found.iter().map(|f| &f.path).collect::<Vec<_>>(),
        vec![&repo.join("CLAUDE.md")],
        "{found:?}"
    );
}

/// A submodule's `.git` FILE points under `<super>/.git/modules/`, not at a
/// `worktrees/` entry: it is its own project, and the superproject's
/// `CLAUDE.md` is an ancestor. Guards Fix 1 against treating every `.git` file
/// as a linked worktree.
#[test]
fn a_submodule_reports_its_superprojects_claude_md() {
    let tmp = TempDir::new().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let upstream = base.join("upstream");
    committed_repo(&upstream, "README.md", "# up\n");
    let sup = base.join("super");
    committed_repo(&sup, "CLAUDE.md", "# Super\n\nSuperproject notes.\n");
    git(
        &sup,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            upstream.to_str().unwrap(),
            "sm",
        ],
    );
    let sm = sup.join("sm");
    assert!(
        sm.join(".git").is_file(),
        "fixture: a submodule has a .git file"
    );

    assert_eq!(resolve_project_root(&sm, None).unwrap(), sm);
    let found = scan(&sm, None, None).unwrap().found;
    assert_eq!(
        found.iter().map(|f| &f.path).collect::<Vec<_>>(),
        vec![&sup.join("CLAUDE.md")],
        "{found:?}"
    );
}
