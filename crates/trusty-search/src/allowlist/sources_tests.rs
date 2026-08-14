//! Unit tests for the #767 allowlist union: explicit entries, project-registry
//! roots, provisioned worktrees, and the denylist's precedence over all three.
//!
//! Why: the union is the security control. Each member needs its own proof that
//! it admits what it should, and — more importantly — that it admits nothing
//! else. Every test injects both file paths, so none of them reads the
//! developer's real configuration.
//! What: fixtures build an `AllowlistPaths` over a `tempfile::TempDir`, using
//! `$HOME/.trusty-search-allowlist-tests` for roots that must survive the hard
//! denylist (a `TempDir` under `/var/folders` would be denied by prefix).
//! Test: this file.

use std::path::{Path, PathBuf};

use super::sources::{
    default_project_paths_file, project_roots, resolve_allow_source, AllowSource, AllowlistPaths,
};
use super::{check_path_with, AllowlistConfig, AllowlistEntry, AllowlistVerdict};

/// A directory that passes the hard denylist, unlike a `/var/folders` tempdir.
///
/// Why: `SENSITIVE_PATH_PREFIXES` denies the OS temp dir outright, so a root
/// built with `tempfile::tempdir()` would be refused before the union is ever
/// consulted and the test would pass for the wrong reason.
/// What: `$HOME/.trusty-search-allowlist-tests/<name>-<pid>`, created fresh.
fn safe_root(name: &str) -> PathBuf {
    let base = dirs::home_dir()
        .expect("HOME required")
        .join(".trusty-search-allowlist-tests");
    let dir = base.join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test root");
    std::fs::canonicalize(&dir).expect("canonicalize test root")
}

/// Fixture paths: a config dir for `allowlist.toml` and `projects.json`.
fn fixture(dir: &Path) -> AllowlistPaths {
    AllowlistPaths::default()
        .with_allowlist(dir.join("allowlist.toml"))
        .with_project_paths(dir.join("projects.json"))
}

fn write_projects(paths: &AllowlistPaths, roots: &[&Path]) {
    let rows: Vec<serde_json::Value> = roots
        .iter()
        .map(|p| serde_json::json!({ "alias": "x", "path": p }))
        .collect();
    let file = paths.project_paths_file();
    std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
    std::fs::write(&file, serde_json::to_string(&rows).expect("json")).expect("write projects");
}

fn write_allowlist(paths: &AllowlistPaths, roots: &[&Path]) {
    let cfg = AllowlistConfig {
        entries: roots
            .iter()
            .map(|p| AllowlistEntry {
                path: p.to_path_buf(),
                name: None,
                exclude: Vec::new(),
                extensions: Vec::new(),
                skip_kg: false,
            })
            .collect(),
    };
    cfg.save_to(&paths.allowlist_file())
        .expect("write allowlist");
}

// ── default location ─────────────────────────────────────────────────────────

/// The project registry's default location must end at the file `tm` writes.
///
/// Why: a wrong default silently denies every project — the failure mode is a
/// dead gate, not an error.
#[test]
fn default_project_paths_file_ends_at_the_registry() {
    let p = default_project_paths_file();
    assert!(
        p.ends_with("project-paths.json"),
        "expected the tm project registry filename, got {}",
        p.display()
    );
}

// ── project_roots: fail closed ───────────────────────────────────────────────

/// A registry file that does not exist approves nothing.
#[test]
fn project_roots_empty_when_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(project_roots(&dir.path().join("absent.json")).is_empty());
}

/// A registry file that will not parse approves nothing — an unreadable policy
/// must never read as "allow".
#[test]
fn project_roots_empty_when_malformed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("projects.json");
    std::fs::write(&file, "{ this is not json").expect("write");
    assert!(project_roots(&file).is_empty());
}

/// A well-formed registry yields its paths, ignoring fields this reader does
/// not model.
#[test]
fn project_roots_reads_registry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("projects.json");
    std::fs::write(
        &file,
        r#"[{"alias":"a","path":"/srv/a"},{"alias":"b","path":"/srv/b","future":1}]"#,
    )
    .expect("write");
    assert_eq!(
        project_roots(&file),
        vec![PathBuf::from("/srv/a"), PathBuf::from("/srv/b")]
    );
}

// ── resolve_allow_source ─────────────────────────────────────────────────────

/// Nothing approves an arbitrary root: default-deny.
#[test]
fn unlisted_root_is_denied() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let root = safe_root("unlisted");
    assert_eq!(resolve_allow_source(&root, &paths).expect("check"), None);
}

/// An `allowlist.toml` entry approves its root, reported as `Explicit`.
#[test]
fn resolve_source_reports_explicit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let root = safe_root("explicit");
    write_allowlist(&paths, &[&root]);
    assert_eq!(
        resolve_allow_source(&root, &paths).expect("check"),
        Some(AllowSource::Explicit)
    );
}

/// A registered project approves its root with no `allowlist.toml` at all —
/// this is the half of #767's policy the operator never hand-maintains.
#[test]
fn resolve_source_reports_project() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let root = safe_root("project");
    write_projects(&paths, &[&root]);
    assert_eq!(
        resolve_allow_source(&root, &paths).expect("check"),
        Some(AllowSource::Project)
    );
}

/// When both members list a root, the explicit entry is what gets reported —
/// the operator's own file wins the attribution, because removing it is the
/// action they would take.
#[test]
fn approved_roots_prefers_explicit_over_project() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let root = safe_root("both");
    write_allowlist(&paths, &[&root]);
    write_projects(&paths, &[&root]);
    let roots = super::sources::approved_roots(&paths).expect("roots");
    let matched: Vec<_> = roots.iter().filter(|(p, _)| *p == root).collect();
    assert_eq!(matched.len(), 1, "root must appear exactly once: {roots:?}");
    assert_eq!(matched[0].1, AllowSource::Explicit);
}

// ── provisioned worktrees ────────────────────────────────────────────────────

/// A worktree provisioned under an approved root is approved by derivation —
/// agent worktrees are created and destroyed far too often to hand-approve.
#[test]
fn worktree_under_approved_root_is_allowed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let root = safe_root("wt-parent");
    write_projects(&paths, &[&root]);

    for rel in [".claude/worktrees/agent-abc", ".worktrees/feature-x"] {
        let wt = root.join(rel);
        std::fs::create_dir_all(&wt).expect("mkdir worktree");
        assert_eq!(
            resolve_allow_source(&wt, &paths).expect("check"),
            Some(AllowSource::ProvisionedWorktree {
                parent: root.clone()
            }),
            "{rel} must be approved by derivation"
        );
    }
}

/// A subdirectory of an approved root IS approved, as `WithinApproved`.
///
/// Why: `trusty-search.yaml` declares several named indexes over sub-roots of
/// one repo, and a reindex `root_path` override can narrow an index onto one.
/// Containment costs nothing in blast radius — everything under the approved
/// root is already indexable through the root itself.
#[test]
fn subdirectory_of_approved_root_is_allowed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let root = safe_root("contained");
    write_projects(&paths, &[&root]);

    for rel in ["src", "services/api"] {
        let sub = root.join(rel);
        std::fs::create_dir_all(&sub).expect("mkdir");
        assert_eq!(
            resolve_allow_source(&sub, &paths).expect("check"),
            Some(AllowSource::WithinApproved {
                parent: root.clone()
            }),
            "{rel} must be approved by containment"
        );
    }
}

/// A SIBLING of an approved root is not approved. Containment is a prefix over
/// path COMPONENTS, so `<root>-scratch` next to `<root>` gains nothing.
#[test]
fn sibling_of_approved_root_is_not_allowed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let root = safe_root("sib-approved");
    let sibling = safe_root("sib-approved-scratch");
    write_projects(&paths, &[&root]);
    assert_eq!(resolve_allow_source(&sibling, &paths).expect("check"), None);
}

/// A sensitive directory INSIDE an approved root is still refused — containment
/// widens the allowlist, never the denylist.
#[test]
fn sensitive_dir_inside_approved_root_is_still_denied() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let root = safe_root("contains-secrets");
    write_projects(&paths, &[&root]);
    let secrets = root.join("secrets");
    std::fs::create_dir_all(&secrets).expect("mkdir");
    match check_path_with(&secrets, &paths).expect("check") {
        AllowlistVerdict::Denied { .. } => {}
        other => panic!("expected Denied for <approved>/secrets, got {other:?}"),
    }
}

// ── denylist precedence ──────────────────────────────────────────────────────

/// The hard denylist beats every member of the union. A sensitive root stays
/// refused even when explicitly allowlisted AND registered as a project.
#[test]
fn denylist_wins_over_project_registration() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let sensitive = dirs::home_dir().expect("home").join(".ssh");
    write_allowlist(&paths, &[&sensitive]);
    write_projects(&paths, &[&sensitive]);
    match check_path_with(&sensitive, &paths).expect("check") {
        AllowlistVerdict::Denied { reason } => {
            assert!(reason.contains("refused"), "reason: {reason}");
        }
        other => panic!("expected Denied for ~/.ssh, got {other:?}"),
    }
}

/// `check_path_with` reports the approving source on the allowed arm.
#[test]
fn check_path_with_reports_project_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let root = safe_root("verdict-project");
    write_projects(&paths, &[&root]);
    assert_eq!(
        check_path_with(&root, &paths).expect("check"),
        AllowlistVerdict::Allowed(AllowSource::Project)
    );
}

/// `check_path_with` denies an unapproved-but-safe root — the default-deny core
/// of #767.
#[test]
fn check_path_with_denies_unlisted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    let root = safe_root("verdict-unlisted");
    assert_eq!(
        check_path_with(&root, &paths).expect("check"),
        AllowlistVerdict::NotAllowlisted
    );
}

/// A corrupt `allowlist.toml` is an error, never an empty allowlist. Callers
/// must refuse; silently reading it as "no entries" would be indistinguishable
/// from a working default-deny while actually being a broken policy.
#[test]
fn malformed_allowlist_is_an_error_not_an_empty_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = fixture(dir.path());
    std::fs::write(paths.allowlist_file(), "this is not toml [[[").expect("write");
    let root = safe_root("verdict-corrupt");
    assert!(check_path_with(&root, &paths).is_err());
}
