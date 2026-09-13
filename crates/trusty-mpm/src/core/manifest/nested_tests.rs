//! Tests for the bounded nested-manifest walk (#7781).
//!
//! Why: every bound in `super` fails closed by returning "nothing found", which
//! is indistinguishable from a genuinely empty tree unless each one is pinned.
//! The anchor derivation carries the same risk in reverse: it silently stops
//! finding a stack when a manifest marker gains a shape it does not handle.
//! What: anchor derivation against the BUNDLED manifest, the depth, scanned-
//! directory and shared-member bounds and which of the two flags each one sets
//! — `truncated` for a resource cap, `depth_limited` for the declared depth,
//! each alone and both in one walk, with the WARN and DEBUG line each emits —
//! the skip rules, `.gitignore` parsing, determinism, symlink containment, and
//! an unreadable subdirectory.
//! Test: this file.

use super::*;
use crate::core::manifest::framework::framework_agent_categories;
use std::fs;
use tempfile::TempDir;

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

/// The bundled manifest's anchors — the set every detection call really uses.
fn bundled_anchors() -> MarkerAnchors {
    let categories = framework_agent_categories().expect("bundled manifest must be valid");
    MarkerAnchors::from_categories(&categories)
}

/// Nested roots relative to `root`, for readable assertions.
fn nested(root: &Path) -> Vec<String> {
    let budget = ProbeBudget::new();
    nested_probe_roots(root, &bundled_anchors(), &budget, &[root.to_path_buf()])
        .roots
        .into_iter()
        .map(|p| {
            p.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

/// Every marker the bundled manifest declares yields an anchor that matches it.
///
/// Why: the walk finds a nested project only if the marker's file name is an
/// anchor. A marker shape this derivation mishandles would silently stop the
/// stack from ever being detected below the root — the failure mode #7781 is
/// closing, reintroduced one marker at a time.
/// What: for every declared marker, strips the content-probe tail, takes the
/// first path segment, and asserts [`MarkerAnchors::matches`] accepts it.
/// Test: this function IS the test.
#[test]
fn anchors_cover_every_bundled_marker() {
    let categories = framework_agent_categories().expect("bundled manifest must be valid");
    let anchors = MarkerAnchors::from_categories(&categories);
    let declared = categories
        .language
        .iter()
        .chain(categories.framework.iter())
        .chain(categories.platform.iter());
    for entry in declared {
        for marker in &entry.markers {
            let head = marker
                .split("::")
                .next()
                .unwrap()
                .split('/')
                .next()
                .unwrap()
                .to_string();
            assert!(
                anchors.matches(&head),
                "`{marker}` (declared by {}) must yield a matching anchor",
                entry.stem
            );
        }
    }
}

/// The `*.csproj` family anchors as a glob, not as a literal name.
#[test]
fn anchor_globs_match_dotnet() {
    let anchors = bundled_anchors();
    assert!(anchors.matches("MyApp.csproj"));
    assert!(anchors.matches("MyApp.sln"));
    assert!(
        !anchors.matches("MyApp.csproj.bak"),
        "the glob is suffix-anchored, as `marker_present` is"
    );
}

/// An undeclared nested UI package is found (the trusty-tools shape).
///
/// Why: the concrete case behind the owner's 2026-09-13 ruling — a Cargo
/// workspace whose Svelte app sits in `crates/<crate>/ui`, declared by no root
/// manifest key, so nothing but a walk can see it.
/// What: builds that layout and asserts the UI directory is returned.
/// Test: this function IS the test.
#[test]
fn nested_ui_package_is_found() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "Cargo.toml", "[workspace]\nmembers = []\n");
    write(tmp.path(), "crates/app/Cargo.toml", "[package]\n");
    write(tmp.path(), "crates/app/ui/package.json", "{}");

    assert_eq!(nested(tmp.path()), vec!["crates/app", "crates/app/ui"]);
}

/// The walk stops at `MAX_NESTED_DEPTH`, and says so as a DEPTH limit.
///
/// Why (#7781 round-3): the depth bound is the design's declared scope and
/// trips on most real repositories, so reporting it as `truncated` made that
/// flag near-universally true and useless as the #7751 fail-closed signal.
/// What: a tree with an anchored directory one level past the bound; asserts
/// the walk reaches depth 4, refuses depth 5, and reports `depth_limited`
/// WITHOUT `truncated`.
/// Test: this function IS the test.
#[test]
fn depth_bound_stops_the_walk() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a/b/c/d/package.json", "{}");
    write(tmp.path(), "a/b/c/d/e/package.json", "{}");

    let budget = ProbeBudget::new();
    let probe = nested_probe_roots(
        tmp.path(),
        &bundled_anchors(),
        &budget,
        &[tmp.path().to_path_buf()],
    );
    assert!(
        probe.roots.contains(&tmp.path().join("a/b/c/d")),
        "depth 4 is walked"
    );
    assert!(
        !probe.roots.contains(&tmp.path().join("a/b/c/d/e")),
        "depth 5 is past the bound and must not be probed"
    );
    assert!(
        probe.depth_limited,
        "leaving a child of the deepest walked directory unread is the depth limit (#7781)"
    );
    assert!(
        !probe.truncated,
        "the depth bound is the declared scope, never a resource cap (#7781 round-3)"
    );
}

/// A tree stopped by the depth bound alone reports no resource truncation.
///
/// Why (#7781 round-3): this is the discrimination the split exists for. A
/// consumer that fails closed on `truncated` (#7751) must not be tripped by an
/// ordinary deep repository, and this test fails if the depth site ever sets
/// `truncated` again.
/// What: an anchored package nested past the bound, with no resource cap in
/// reach; asserts `depth_limited` is set, `truncated` is clear, and the
/// packages within the bound are still returned.
/// Test: this function IS the test.
#[test]
fn depth_limited_tree_is_not_truncated() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a/package.json", "{}");
    write(tmp.path(), "a/b/c/d/e/package.json", "{}");

    let budget = ProbeBudget::new();
    let probe = nested_probe_roots(
        tmp.path(),
        &bundled_anchors(),
        &budget,
        &[tmp.path().to_path_buf()],
    );
    assert_eq!(
        probe.roots,
        vec![tmp.path().join("a")],
        "everything within the bound is still found"
    );
    assert!(probe.depth_limited, "the walk left depth-5 children unread");
    assert!(
        !probe.truncated,
        "no resource cap was exhausted, so the fail-closed flag stays clear"
    );
}

/// Build output, dependency, and fixture trees are never descended into.
///
/// Why: a marker under any of them describes a third party or a test fixture,
/// not this project's stack. Measured on trusty-tools, `testdata/csharp` alone
/// put `dotnet-engineer` in a Rust workspace's roster (#7781).
/// What: seeds a `package.json` one level inside each skipped name and asserts
/// the walk returns nothing at all.
/// Test: this function IS the test.
#[test]
fn skipped_directories_are_never_probed() {
    let tmp = TempDir::new().unwrap();
    for skipped in [
        "node_modules",
        "target",
        "dist",
        "build",
        "vendor",
        "testdata",
        "test-data",
        "fixtures",
        ".git",
    ] {
        write(tmp.path(), &format!("{skipped}/pkg/package.json"), "{}");
    }
    assert!(
        nested(tmp.path()).is_empty(),
        "no skipped tree may contribute a probe root"
    );
}

/// A directory the root `.gitignore` names is skipped.
#[test]
fn gitignored_directory_is_skipped() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), ".gitignore", "generated/\n");
    write(tmp.path(), "generated/app/package.json", "{}");
    write(tmp.path(), "kept/package.json", "{}");

    assert_eq!(nested(tmp.path()), vec!["kept"]);
}

/// Only bare `.gitignore` names are honoured; path and glob rules are left to git.
///
/// Why: the parse is deliberately narrow, so both what it accepts and what it
/// drops need pinning. #7781 round-2 added the `**/<name>/` form — that IS
/// "this bare name at any depth", exactly what the skip set means, and most
/// real `.gitignore` files spell it that way, so dropping it lost the rule.
/// What: asserts `**/target/` and `**/out/` yield their bare names, that `/out`
/// and `build-cache/` still do, and that a real PATH rule (`src/generated`), a
/// glob (`*.log`), a negation, a comment, and a deeper `**/a/b/` are all
/// dropped. `/out` and `**/out/` both mean `out`; the caller collects the
/// result into a set, so the repeat is expected rather than de-duplicated here.
/// Test: this function IS the test.
#[test]
fn gitignore_path_rules_are_ignored() {
    let names = gitignore_dir_names(
        "# comment\n\n!kept\n**/target/\n/out\nbuild-cache/\n**/out/\nsrc/generated\n*.log\n**/a/b/\n",
    );
    assert_eq!(
        names,
        vec![
            "target".to_string(),
            "out".to_string(),
            "build-cache".to_string(),
            "out".to_string(),
        ]
    );
}

/// The walk returns a stable order regardless of `read_dir` order.
#[test]
fn walk_is_deterministic() {
    let tmp = TempDir::new().unwrap();
    for name in ["zeta", "alpha", "mid"] {
        write(tmp.path(), &format!("{name}/package.json"), "{}");
    }
    let first = nested(tmp.path());
    assert_eq!(first, vec!["alpha", "mid", "zeta"]);
    assert_eq!(nested(tmp.path()), first, "repeat runs agree");
}

/// A root that does not exist is absence, never an error.
///
/// Why: renamed in #7781 round-2 — the old name claimed unreadable-directory
/// coverage this body never had (it passes a MISSING path, which fails at a
/// different `read_dir` errno and never exercises a permission denial). The
/// real unreadable case is `unreadable_subdirectory_is_skipped_not_an_error`.
/// What: walks a path that was never created and asserts an empty, untruncated
/// result rather than a panic.
/// Test: this function IS the test.
#[test]
fn missing_root_is_not_an_error() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("gone");
    let budget = ProbeBudget::new();
    let probe = nested_probe_roots(&missing, &bundled_anchors(), &budget, &[]);
    assert!(probe.roots.is_empty());
    assert!(
        !probe.truncated,
        "an empty tree is read in full, not truncated"
    );
}

/// An unreadable SUBDIRECTORY is skipped; its readable sibling is still returned.
///
/// Why: `scan_dir` swallows a `read_dir` error to fail closed, and #7781
/// round-2 found nothing proved that the swallow is LOCAL — one `chmod 000`
/// directory must not abort the walk and take every sibling's stack with it.
/// What: `chmod 0o000` on one anchored subdirectory, an anchored sibling beside
/// it; asserts the walk returns the sibling, does not panic, and reports no
/// truncation (a denied directory is absence, not a tripped bound). Unix only:
/// Windows has no equivalent mode, and the precedent for this fixture is
/// `session_manager::worktree_git_fixture::deny_all`.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn unreadable_subdirectory_is_skipped_not_an_error() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "denied/app/package.json", "{}");
    write(tmp.path(), "readable/package.json", "{}");

    let denied = tmp.path().join("denied");
    let restore = fs::metadata(&denied).unwrap().permissions();
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();

    let budget = ProbeBudget::new();
    let probe = nested_probe_roots(
        tmp.path(),
        &bundled_anchors(),
        &budget,
        &[tmp.path().to_path_buf()],
    );

    // Restore before asserting so a failure still leaves TempDir removable.
    fs::set_permissions(&denied, restore).unwrap();

    let found: Vec<String> = probe
        .roots
        .iter()
        .map(|p| p.strip_prefix(tmp.path()).unwrap().to_string_lossy().into())
        .collect();
    assert_eq!(
        found,
        vec!["readable".to_string()],
        "an unreadable directory must cost only itself, never its siblings"
    );
    assert!(
        !probe.truncated,
        "a permission denial is absence, not a tripped scan bound"
    );
}

/// A symlink to an outside directory is not traversed (#7781 round-2).
///
/// Why: `entry.path().is_dir()` FOLLOWS a symlink, so a link to a directory
/// outside the project made that outside tree's manifests part of this
/// project's detected stack — and a link pointing at an ancestor would revisit
/// the same subtree under a second name. `super::workspace` already refuses a
/// declared member pattern that escapes the root; the walk must match it.
/// What: an out-of-tree directory holding `package.json`, symlinked into the
/// project, plus a real anchored directory. Asserts only the real one is
/// returned. Unix only — symlink creation needs no privilege there.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn directory_symlink_is_not_traversed() {
    let outside = TempDir::new().unwrap();
    write(outside.path(), "escaped/package.json", "{}");

    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "real/package.json", "{}");
    std::os::unix::fs::symlink(outside.path().join("escaped"), tmp.path().join("linked")).unwrap();

    assert_eq!(
        nested(tmp.path()),
        vec!["real"],
        "a directory symlink must contribute no probe root — the walk cannot leave the project"
    );
}

/// Exceeding `MAX_SCANNED_DIRS` reports truncation and says which bound tripped.
///
/// Why (#7781 round-2 HIGH and MEDIUM): the directory-count bound had no test
/// at all, and it fails closed by returning a SHORT list that looks exactly
/// like a small repo. Both halves are pinned here — the flag the caller reads
/// and the WARN an operator reads.
/// What: builds `FANOUT` × `FANOUT` directories at depths 1 and 2, so the walk
/// makes `1 + 64 + 4096 = 4161` `read_dir` calls against a bound of 4096, and
/// captures the walk's `tracing` output in a `LogBuffer`. Asserts `truncated`
/// AND `!depth_limited` — the two-tier fixture never reaches the depth bound, so
/// this pins the resource cap alone (#7781 round-3) — and that the WARN names
/// `MAX_SCANNED_DIRS` so a different bound cannot satisfy this test.
/// `#[serial]` because installing a subscriber perturbs the process-global
/// interest cache.
/// Test: this function IS the test.
#[test]
#[serial_test::serial]
fn scanned_dirs_bound_reports_truncation() {
    use tracing_subscriber::layer::SubscriberExt;

    // #4931: a thread-local subscriber never raises the process-global level,
    // so without this the capture below is empty unless an unrelated test in
    // the binary installed a global default first.
    crate::test_support::enable_event_capture();
    const FANOUT: usize = 64;
    // The root, the first tier, and the second tier are all scanned. Checked at
    // COMPILE time, so raising MAX_SCANNED_DIRS without resizing the fixture
    // fails the build rather than quietly turning this test into a no-op.
    const {
        assert!(
            1 + FANOUT + FANOUT * FANOUT > MAX_SCANNED_DIRS,
            "the fixture below must exceed the bound it is pinning"
        )
    };
    let tmp = TempDir::new().unwrap();
    for outer in 0..FANOUT {
        for inner in 0..FANOUT {
            fs::create_dir_all(tmp.path().join(format!("d{outer}/d{inner}"))).unwrap();
        }
    }

    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let probe = tracing::subscriber::with_default(subscriber, || {
        let budget = ProbeBudget::new();
        nested_probe_roots(
            tmp.path(),
            &bundled_anchors(),
            &budget,
            &[tmp.path().to_path_buf()],
        )
    });

    assert!(
        probe.truncated,
        "a walk stopped by MAX_SCANNED_DIRS is truncated"
    );
    assert!(
        !probe.depth_limited,
        "a two-tier fixture never reaches the depth bound (#7781 round-3)"
    );
    let lines = buffer.tail(64);
    assert!(
        lines.iter().any(|l| l.contains("MAX_SCANNED_DIRS")),
        "the WARN must name the bound that tripped, or an operator cannot tell \
         which ceiling to raise: {lines:#?}"
    );
}

/// A resource cap and the depth bound can trip in the SAME walk (#7781 round-3).
///
/// Why: round-2 review — the two flags are documented as independent ("either,
/// both, or neither"), and only the single-flag cases were pinned. The walk's
/// resource-cap exits carried `depth_limited` out by hand, so a walk that hit
/// both bounds would have reported the resource cap alone had either exit
/// dropped it, and no test would have noticed.
/// The LOG half had the same gap for real: the resource-cap exits returned
/// before `debug_depth_limited`, so this walk set `depth_limited` and logged
/// nothing about it. Both halves are asserted here.
/// What: one chain to depth 3, then a depth-4 tier wide enough to exhaust
/// `MAX_SCANNED_DIRS`. Children are sorted, so the first depth-4 directory is
/// scanned long before the cap trips, and its unread child sets the depth flag;
/// the cap then ends the walk. Asserts both flags, and that BOTH the WARN and
/// the DEBUG reach a subscriber. `#[serial]` for the same subscriber reason as
/// `scanned_dirs_bound_reports_truncation`.
/// Test: this function IS the test.
#[test]
#[serial_test::serial]
fn both_bounds_can_trip_together() {
    use tracing_subscriber::layer::SubscriberExt;

    crate::test_support::enable_event_capture();
    let tmp = TempDir::new().unwrap();
    // Depths 1-3 are a single chain, so the scan budget is spent at depth 4:
    // 4 chain directories plus MAX_SCANNED_DIRS leaves exceeds the bound.
    let tier = tmp.path().join("a/b/c");
    for i in 0..MAX_SCANNED_DIRS {
        fs::create_dir_all(tier.join(format!("d{i:05}"))).unwrap();
    }
    // The FIRST depth-4 directory in sorted order has a child the walk cannot
    // read, which is the depth limit — reached before the scan cap trips.
    fs::create_dir_all(tier.join("d00000/unread")).unwrap();

    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let probe = tracing::subscriber::with_default(subscriber, || {
        let budget = ProbeBudget::new();
        nested_probe_roots(
            tmp.path(),
            &bundled_anchors(),
            &budget,
            &[tmp.path().to_path_buf()],
        )
    });

    assert!(
        probe.depth_limited,
        "a child left unread at the declared depth is the depth limit, whatever \
         ended the walk afterwards"
    );
    assert!(
        probe.truncated,
        "the depth-4 tier is wider than MAX_SCANNED_DIRS, so the resource cap \
         ended the walk"
    );
    let lines = buffer.tail(64);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("WARN") && l.contains("MAX_SCANNED_DIRS")),
        "the resource cap still names itself: {lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("DEBUG") && l.contains("stopped at its declared depth")),
        "the depth limit must be logged even when a resource cap ended the walk — \
         the early returns used to skip it: {lines:#?}"
    );
}

/// The depth limit reaches an operator's log, at DEBUG and never as a WARN.
///
/// Why (#7781 round-3 review): `debug_depth_limited` was called from exactly one
/// of the walk's three exits, so a walk that ended at a resource cap set the
/// flag and logged nothing. The WARN half of this contract is pinned by
/// `scanned_dirs_bound_reports_truncation`; this is its DEBUG counterpart.
/// What: a package nested past `MAX_NESTED_DEPTH` with no resource cap in reach;
/// captures the walk's `tracing` output in a `LogBuffer` and asserts one DEBUG
/// line naming the declared depth, and no WARN. `#[serial]` because installing a
/// subscriber perturbs the process-global interest cache, and
/// `enable_event_capture` because the process-global level starts at `OFF` and
/// only a GLOBAL default raises it — without it this capture is empty whenever
/// no unrelated test happened to install one first (#4931, measured here).
/// Test: this function IS the test.
#[test]
#[serial_test::serial]
fn depth_limit_is_logged_at_debug() {
    use tracing_subscriber::layer::SubscriberExt;

    crate::test_support::enable_event_capture();
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a/b/c/d/e/package.json", "{}");

    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let probe = tracing::subscriber::with_default(subscriber, || {
        let budget = ProbeBudget::new();
        nested_probe_roots(
            tmp.path(),
            &bundled_anchors(),
            &budget,
            &[tmp.path().to_path_buf()],
        )
    });

    assert!(
        probe.depth_limited,
        "the fixture nests past the depth bound"
    );
    let lines = buffer.tail(64);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("DEBUG") && l.contains("stopped at its declared depth")),
        "the depth limit must reach the log, or only the flag records it: {lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("WARN")),
        "the declared depth is not a resource cap and must not warn: {lines:#?}"
    );
}

/// A tree the walk reads in full reports no truncation.
///
/// Why: the counterpart to `scanned_dirs_bound_reports_truncation`. A flag that
/// is always true carries no information, and the #7751 consumer that will fail
/// closed on it would then refuse every project.
/// What: a small, shallow tree with one nested package; asserts the package is
/// found and `truncated` is false.
/// Test: this function IS the test.
#[test]
fn a_small_tree_is_not_truncated() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "pkg/package.json", "{}");

    let budget = ProbeBudget::new();
    let probe = nested_probe_roots(
        tmp.path(),
        &bundled_anchors(),
        &budget,
        &[tmp.path().to_path_buf()],
    );
    assert_eq!(probe.roots, vec![tmp.path().join("pkg")]);
    assert!(
        !probe.truncated,
        "no resource cap is tripped by a two-directory tree"
    );
    assert!(
        !probe.depth_limited,
        "a two-directory tree is read to its own bottom, well inside the depth bound"
    );
}

/// The member cap counts declared members and nested finds together.
///
/// Why: the two discovery paths share one ceiling, so a monorepo cannot exceed
/// it by splitting its projects across both.
/// What: passes an `already` list at the cap and asserts the walk adds nothing
/// even though an anchored directory is present.
/// Test: this function IS the test.
#[test]
fn member_cap_is_shared() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "pkg/package.json", "{}");
    let already: Vec<PathBuf> = (0..=MAX_WORKSPACE_MEMBERS)
        .map(|i| tmp.path().join(format!("filler{i}")))
        .collect();
    let budget = ProbeBudget::new();
    let probe = nested_probe_roots(tmp.path(), &bundled_anchors(), &budget, &already);
    assert!(
        probe.roots.is_empty(),
        "at the shared cap the walk contributes nothing"
    );
    assert!(
        probe.truncated,
        "the member cap is a RESOURCE cap: hitting it is truncation, not an empty tree (#7781)"
    );
}
