//! Unit tests for the #6926 incremental directory-size index.
//!
//! Why: "it caches" and "it refreshes incrementally" are both invisible in the
//! byte total — the same number comes back whether the index walked once or a
//! thousand times. Every claim here is therefore asserted against
//! [`super::IndexStats`]'s walk counters, so removing the caching fails a test
//! rather than merely making the dashboard slow.
//! What: the deterministic total, the zero-syscall cache hit, incremental
//! re-read of only the changed subtree, the node backstop, deletion sweeping,
//! the two refusals, symlink exclusion, the depth cap, and a refused subtree.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use super::*;
use std::fs;

/// A policy with no forbidden roots, so tests never touch `$HOME` or `/`.
///
/// Temp directories can legitimately sit under `$HOME` on some machines, so the
/// default allowlist is replaced outright rather than worked around.
fn test_policy(max_age: Duration, node_max_age: Duration) -> IndexPolicy {
    IndexPolicy {
        max_age,
        node_max_age,
        max_depth: DEFAULT_MAX_DEPTH,
        walk_budget: DEFAULT_WALK_BUDGET,
        forbidden_roots: Vec::new(),
    }
}

/// `max_age` of zero forces a refresh on every `measure`, which is how the
/// incremental tests observe a walk at all.
fn always_refresh() -> IndexPolicy {
    test_policy(Duration::ZERO, DEFAULT_NODE_MAX_AGE)
}

/// Write a file of exactly `bytes` bytes, creating parents as needed.
fn write_file(path: &Path, bytes: usize) {
    fs::create_dir_all(path.parent().expect("path has a parent")).expect("create parents");
    fs::write(path, vec![b'x'; bytes]).expect("write file");
}

/// `root/f1`=10, `root/a/f2`=20, `root/a/deep/f3`=30, `root/b/f4`=40 — 100 total
/// across four directories.
fn known_tree(root: &Path) {
    write_file(&root.join("f1"), 10);
    write_file(&root.join("a/f2"), 20);
    write_file(&root.join("a/deep/f3"), 30);
    write_file(&root.join("b/f4"), 40);
}

/// The byte total itself must be exact, not approximate — the sunburst's arc
/// angles are proportional to it.
#[test]
fn bytes_sum_the_files_in_a_known_tree() {
    let tmp = tempfile::tempdir().expect("tempdir");
    known_tree(tmp.path());

    let mut index = DirSizeIndex::with_policy(always_refresh());
    let size = index.measure(tmp.path()).expect("measure");

    assert_eq!(size.bytes, 100, "10 + 20 + 30 + 40");
    assert!(!size.truncated);
    assert!(size.unreadable.is_empty());
    assert!(!size.from_cache);
    assert_eq!(index.stats().directories_read, 4, "root, a, a/deep, b");
}

/// The regression test for the whole issue: a second read inside the cadence
/// must cost NOTHING. Delete the root cache and this fails — `directories_read`
/// climbs to 8 and `cache_hits` stays at 0.
#[test]
fn a_second_read_inside_the_cadence_performs_no_walk() {
    let tmp = tempfile::tempdir().expect("tempdir");
    known_tree(tmp.path());

    let mut index = DirSizeIndex::with_policy(test_policy(DEFAULT_MAX_AGE, DEFAULT_NODE_MAX_AGE));
    let first = index.measure(tmp.path()).expect("first measure");
    let after_first = index.stats();

    let second = index.measure(tmp.path()).expect("second measure");
    let after_second = index.stats();

    assert_eq!(
        after_second.directories_read, after_first.directories_read,
        "a cached read must not open a single directory"
    );
    assert_eq!(
        after_second.directories_revalidated, after_first.directories_revalidated,
        "a cached read must not even stat a directory"
    );
    assert_eq!(after_second.refreshes, after_first.refreshes);
    assert_eq!(after_second.cache_hits, 1);
    assert_eq!(second.bytes, first.bytes);
    assert!(second.from_cache);
    assert_eq!(
        second.measured_at, first.measured_at,
        "a cached figure reports when it was MEASURED, not when it was read"
    );
}

/// The incremental claim: after one subtree changes, only that subtree's
/// directory is listed again. Every other directory costs one stat.
#[test]
fn a_changed_subtree_is_the_only_one_re_read() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    write_file(&root.join("a/f"), 10);
    write_file(&root.join("b/f"), 20);
    write_file(&root.join("b/c/f"), 30);

    let mut index = DirSizeIndex::with_policy(always_refresh());
    assert_eq!(index.measure(root).expect("first measure").bytes, 60);
    let before = index.stats();
    assert_eq!(before.directories_read, 4, "root, a, b, b/c");

    // Some filesystems keep mtime at one-second resolution, so a mutation in
    // the same second as the directory's creation would be invisible. Waiting
    // past that boundary is what makes the mtime signal observable everywhere,
    // not an arbitrary settling delay.
    std::thread::sleep(Duration::from_millis(1100));
    write_file(&root.join("a/new"), 5);

    let after_change = index.measure(root).expect("second measure");
    let after = index.stats();

    assert_eq!(after_change.bytes, 65);
    assert_eq!(
        after.directories_read - before.directories_read,
        1,
        "only `a` changed, so only `a` is listed again"
    );
    assert_eq!(
        after.directories_revalidated - before.directories_revalidated,
        3,
        "root, b and b/c are reused on an unchanged mtime"
    );
}

/// A file that grows in place moves no directory mtime, so the node backstop is
/// the only thing that ever catches it.
#[test]
fn the_node_backstop_re_reads_a_file_that_grew_in_place() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    write_file(&root.join("log.txt"), 10);

    let mut index = DirSizeIndex::with_policy(test_policy(Duration::ZERO, Duration::ZERO));
    assert_eq!(index.measure(root).expect("first measure").bytes, 10);
    let before = index.stats();

    write_file(&root.join("log.txt"), 30);

    assert_eq!(index.measure(root).expect("second measure").bytes, 30);
    assert_eq!(
        index.stats().directories_read - before.directories_read,
        1,
        "an expired node is listed again whatever its mtime says"
    );
}

/// A removed subtree must leave the total AND the node map — a cached node for
/// a deleted worktree would otherwise outlive it.
#[test]
fn a_deleted_subtree_drops_out_of_the_total() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    write_file(&root.join("a/f"), 10);
    write_file(&root.join("b/f"), 20);

    let mut index = DirSizeIndex::with_policy(test_policy(Duration::ZERO, Duration::ZERO));
    assert_eq!(index.measure(root).expect("first measure").bytes, 30);
    assert!(index.nodes.contains_key(&root.join("b")));

    fs::remove_dir_all(root.join("b")).expect("remove b");

    assert_eq!(index.measure(root).expect("second measure").bytes, 10);
    assert!(
        !index.nodes.contains_key(&root.join("b")),
        "the sweep must drop nodes the refresh no longer reaches"
    );
}

/// The Disk dashboard deletes worktrees; a cached total must not outlive one.
#[test]
fn invalidate_forces_the_next_read_to_walk() {
    let tmp = tempfile::tempdir().expect("tempdir");
    known_tree(tmp.path());

    let mut index = DirSizeIndex::with_policy(test_policy(DEFAULT_MAX_AGE, DEFAULT_NODE_MAX_AGE));
    index.measure(tmp.path()).expect("first measure");
    index.measure(tmp.path()).expect("cached measure");
    let before = index.stats();
    assert_eq!(before.cache_hits, 1);

    index.invalidate(tmp.path());
    let after_invalidate = index.measure(tmp.path()).expect("measure after invalidate");

    assert!(!after_invalidate.from_cache);
    assert_eq!(index.stats().cache_hits, before.cache_hits);
    assert_eq!(
        index.stats().directories_read - before.directories_read,
        4,
        "invalidate drops the nodes too, so the next read is a cold walk"
    );
}

/// A link out of a worktree must neither inflate the figure nor be descended
/// into — the loop that would follow is why this is a hard rule, not a default.
#[cfg(unix)]
#[test]
fn a_symlink_is_neither_followed_nor_counted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    write_file(&root.join("real/f"), 100);
    std::os::unix::fs::symlink(root.join("real"), root.join("link")).expect("symlink dir");
    std::os::unix::fs::symlink(root.join("real/f"), root.join("link_f")).expect("symlink file");

    let mut index = DirSizeIndex::with_policy(always_refresh());
    let size = index.measure(root).expect("measure");

    assert_eq!(
        size.bytes, 100,
        "the target is counted once, the links never"
    );
    assert_eq!(index.stats().directories_read, 2, "root and real only");
}

/// A subtree the OS refuses is disclosed and skipped. It must not abort the
/// measurement, because one unreadable directory anywhere in a project would
/// otherwise blank the whole Disk view.
#[cfg(unix)]
#[test]
fn an_unreadable_subtree_is_recorded_not_fatal() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    write_file(&root.join("ok/f"), 100);
    write_file(&root.join("denied/f"), 50);
    let denied = root.join("denied");
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).expect("chmod 000");

    if fs::read_dir(&denied).is_ok() {
        // Running as root, where mode bits are advisory. Restore and skip
        // rather than assert something this process cannot make true.
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o755)).expect("restore");
        return;
    }

    let mut index = DirSizeIndex::with_policy(test_policy(DEFAULT_MAX_AGE, DEFAULT_NODE_MAX_AGE));
    let size = index.measure(root).expect("measure must not fail");
    let cached = index.measure(root).expect("cached measure");
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o755)).expect("restore");

    assert_eq!(size.bytes, 100, "the readable half is still measured");
    assert_eq!(size.unreadable, vec![denied.clone()]);
    assert!(
        !index.nodes.contains_key(&denied),
        "a refused directory is never cached, so the next refresh retries it"
    );
    assert!(cached.from_cache);
    assert_eq!(
        cached.unreadable, size.unreadable,
        "the refused subtree must survive the cache hit"
    );
}

/// The depth cap must report itself. A silently truncated total is worse than a
/// short one, because nothing downstream can tell it apart from a small tree.
#[test]
fn the_depth_cap_truncates_and_says_so() {
    let tmp = tempfile::tempdir().expect("tempdir");
    known_tree(tmp.path());

    let mut policy = always_refresh();
    policy.max_depth = 1;
    let mut index = DirSizeIndex::with_policy(policy);
    let size = index.measure(tmp.path()).expect("measure");

    assert!(size.truncated, "`a/deep` is below the cap");
    assert_eq!(size.bytes, 70, "f1 + f2 + f4; f3 sits under the cap");
}

/// `/` and `$HOME` are refused by construction, not by a caller remembering to
/// check. The refusal costs no syscall.
#[test]
fn default_policy_forbids_the_filesystem_root() {
    assert!(
        IndexPolicy::default()
            .forbidden_roots
            .contains(&PathBuf::from("/"))
    );

    let mut index = DirSizeIndex::new();
    let err = index.measure(Path::new("/")).expect_err("`/` is refused");

    assert!(matches!(err, SizeIndexError::ForbiddenRoot { .. }));
    assert_eq!(index.stats().refreshes, 0);
}

/// A forbidden root refuses its ANCESTORS too — walking `/Users` walks `$HOME`.
#[test]
fn a_forbidden_root_is_refused_before_any_walk() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    write_file(&home.join("project/f"), 10);

    let mut policy = always_refresh();
    policy.forbidden_roots = vec![home.clone()];
    let mut index = DirSizeIndex::with_policy(policy);

    assert!(matches!(
        index.measure(&home),
        Err(SizeIndexError::ForbiddenRoot { .. })
    ));
    assert!(
        matches!(
            index.measure(tmp.path()),
            Err(SizeIndexError::ForbiddenRoot { .. })
        ),
        "an ancestor of a forbidden root is forbidden too"
    );
    assert_eq!(
        index.stats().refreshes,
        0,
        "both refusals happen before any walk"
    );

    let inside = index.measure(&home.join("project")).expect("below is fine");
    assert_eq!(inside.bytes, 10);
}

/// A `..`-laced spelling names the same directory, so it must be refused the
/// same way. `Path::starts_with` compares components literally, so before the
/// guard resolved both sides this walked the forbidden root's own parent.
#[test]
fn a_dot_dot_path_into_a_forbidden_root_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    write_file(&home.join("project/f"), 10);

    let mut policy = always_refresh();
    policy.forbidden_roots = vec![home.clone()];
    let mut index = DirSizeIndex::with_policy(policy);

    let detour = home.join("project").join("..");
    assert!(
        matches!(
            index.measure(&detour),
            Err(SizeIndexError::ForbiddenRoot { .. })
        ),
        "`home/project/..` IS `home`"
    );

    let above = home.join("project").join("..").join("..");
    assert!(
        matches!(
            index.measure(&above),
            Err(SizeIndexError::ForbiddenRoot { .. })
        ),
        "`home/project/../..` is the forbidden root's parent"
    );

    assert_eq!(
        index.stats().refreshes,
        0,
        "neither spelling may reach a walk"
    );
}

/// The `canonicalize` fallback, tested directly. Every path-based test above
/// resolves through `canonicalize` instead, so this pure function is the only
/// part of the traversal guard nothing else reaches — and it is the part that
/// runs when `canonicalize` cannot (a missing or unreadable ancestor).
#[test]
fn lexical_normalize_folds_dot_and_dot_dot() {
    assert_eq!(
        lexical_normalize(Path::new("/a/b/../../c")),
        PathBuf::from("/c")
    );
    assert_eq!(
        lexical_normalize(Path::new("/a/../..")),
        PathBuf::from("/"),
        "popping past the root stops at the root"
    );
    assert_eq!(
        lexical_normalize(Path::new("/a/./b")),
        PathBuf::from("/a/b"),
        "a `.` segment is dropped"
    );
    assert_eq!(
        lexical_normalize(Path::new("/a/b/")),
        PathBuf::from("/a/b"),
        "a trailing slash changes nothing"
    );
    assert_eq!(
        lexical_normalize(Path::new("/a/b")),
        PathBuf::from("/a/b"),
        "a path with nothing to fold is returned unchanged"
    );
}

/// A cache hit must carry the walk's shortfalls forward. A cached figure that
/// reported itself complete when the walk behind it was truncated would be
/// indistinguishable from a genuinely small tree.
#[test]
fn a_cached_read_still_reports_truncation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    known_tree(tmp.path());

    let mut policy = test_policy(DEFAULT_MAX_AGE, DEFAULT_NODE_MAX_AGE);
    policy.max_depth = 1;
    let mut index = DirSizeIndex::with_policy(policy);

    let first = index.measure(tmp.path()).expect("first measure");
    assert!(first.truncated);
    assert!(!first.from_cache);

    let cached = index.measure(tmp.path()).expect("cached measure");

    assert!(cached.from_cache, "the second read must come from cache");
    assert!(cached.truncated, "truncation must survive the cache hit");
    assert_eq!(cached.bytes, first.bytes);
    assert_eq!(cached.unreadable, first.unreadable);
    assert_eq!(index.stats().cache_hits, 1);
}

/// `node_max_age` is a bound, so both sides of it are pinned. The cached node
/// here reports a byte count that is deliberately wrong, which is what tells the
/// two cases apart: reusing it yields the lie, re-reading yields the truth.
#[test]
fn the_node_backstop_bound_is_exact() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    write_file(&root.join("f"), 10);
    let mtime = fs::symlink_metadata(root)
        .expect("stat root")
        .modified()
        .expect("mtime");

    let planted = |read_at: Instant| DirNode {
        mtime: Some(mtime),
        own_bytes: 999,
        children: Vec::new(),
        read_at,
    };

    let mut inside =
        DirSizeIndex::with_policy(test_policy(Duration::ZERO, Duration::from_secs(600)));
    inside
        .nodes
        .insert(root.to_path_buf(), planted(Instant::now()));
    let reused = inside.measure(root).expect("measure inside the bound");

    assert_eq!(reused.bytes, 999, "a node inside the bound is reused as-is");
    assert_eq!(inside.stats().directories_read, 0);
    assert_eq!(inside.stats().directories_revalidated, 1);

    let mut outside =
        DirSizeIndex::with_policy(test_policy(Duration::ZERO, Duration::from_millis(50)));
    let expired = Instant::now()
        .checked_sub(Duration::from_millis(60))
        .expect("the process started more than 60ms ago");
    outside.nodes.insert(root.to_path_buf(), planted(expired));
    let reread = outside.measure(root).expect("measure outside the bound");

    assert_eq!(reread.bytes, 10, "an expired node is re-read from disk");
    assert_eq!(outside.stats().directories_read, 1);
    assert_eq!(outside.stats().directories_revalidated, 0);
}

/// A file, or a path that is not there at all, is a refusal rather than a
/// zero — `dir_size_bytes` returns 0 for a missing directory, and that
/// ambiguity is exactly what a dashboard cannot render.
#[test]
fn a_file_path_is_not_a_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("f");
    write_file(&file, 10);

    let mut index = DirSizeIndex::with_policy(always_refresh());

    assert!(matches!(
        index.measure(&file),
        Err(SizeIndexError::NotADirectory { .. })
    ));
    assert!(matches!(
        index.measure(&tmp.path().join("missing")),
        Err(SizeIndexError::NotADirectory { .. })
    ));
}
