//! Tests for the authorship artifact (#5453, #6004).

use rusqlite::params;

use super::{
    build_authorship_summary, recorded_repository_names, repository_has_commits,
    AUTHORSHIP_SCHEMA_VERSION,
};
use crate::core::db::Database;

/// Insert one `authors` row — the identity resolver's output — and hand back
/// its id, so a test can link commits to it exactly as `upsert_observed_authors`
/// does at collection time.
fn insert_author(db: &Database, canonical_name: &str, canonical_email: &str) -> i64 {
    db.connection()
        .execute(
            "INSERT INTO authors (canonical_name, canonical_email) VALUES (?1, ?2)",
            params![canonical_name, canonical_email],
        )
        .expect("insert author");
    db.connection().last_insert_rowid()
}

/// Link an already-inserted commit to a resolved author.
fn link_commit(db: &Database, sha: &str, author_id: i64) {
    let updated = db
        .connection()
        .execute(
            "UPDATE commits SET author_id = ?1 WHERE sha = ?2",
            params![author_id, sha],
        )
        .expect("link commit");
    assert_eq!(updated, 1, "the commit to link must exist: {sha}");
}

/// Insert one commit (with its file touches) into the seeded database.
#[allow(clippy::too_many_arguments)]
fn insert_commit(
    db: &Database,
    sha: &str,
    author_name: &str,
    author_email: &str,
    timestamp: &str,
    repository: &str,
    is_merge: bool,
    paths: &[&str],
) {
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                                  repository, is_merge) \
             VALUES (?1, ?2, ?3, ?4, 'msg', ?5, ?6)",
            params![
                sha,
                author_name,
                author_email,
                timestamp,
                repository,
                is_merge as i64
            ],
        )
        .expect("insert commit");
    let commit_id = db.connection().last_insert_rowid();
    for path in paths {
        db.connection()
            .execute(
                "INSERT INTO files (commit_id, path, change_type) VALUES (?1, ?2, 'modified')",
                params![commit_id, path],
            )
            .expect("insert file");
    }
}

/// A single-author repository has bus factor 1 and 100% concentration.
#[test]
fn builds_from_seeded_commits() {
    let db = Database::open_in_memory().expect("open");
    insert_commit(
        &db,
        "a1",
        "Alice",
        "alice@x.com",
        "2026-01-15T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    insert_commit(
        &db,
        "a2",
        "Alice",
        "alice@x.com",
        "2026-02-15T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );

    let summary = build_authorship_summary(db.connection(), "repo").expect("summary");
    assert_eq!(summary.schema_version, AUTHORSHIP_SCHEMA_VERSION);
    assert_eq!(summary.repository, "repo");
    assert_eq!(summary.distinct_authors, 1);
    assert_eq!(summary.bus_factor, 1);
    assert!((summary.top_author_share_pct - 100.0).abs() < f64::EPSILON);
    assert_eq!(summary.single_author_subsystems, vec!["src".to_string()]);
    assert_eq!(summary.monthly_trajectory.len(), 2);
    assert!(!summary.caveats.is_empty());
}

/// (#5453) Bot commits and merge commits are excluded from every figure.
#[test]
fn bots_and_merges_are_excluded() {
    let db = Database::open_in_memory().expect("open");
    insert_commit(
        &db,
        "h1",
        "Alice",
        "alice@x.com",
        "2026-01-15T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    insert_commit(
        &db,
        "b1",
        "dependabot[bot]",
        "dependabot[bot]@users.noreply.github.com",
        "2026-01-16T00:00:00Z",
        "repo",
        false,
        &["deps/lock.json"],
    );
    insert_commit(
        &db,
        "m1",
        "Bob",
        "bob@x.com",
        "2026-01-17T00:00:00Z",
        "repo",
        true,
        &["src/merged.rs"],
    );

    let summary = build_authorship_summary(db.connection(), "repo").expect("summary");
    assert_eq!(summary.distinct_authors, 1, "bot and merge author excluded");
    assert_eq!(summary.single_author_subsystems, vec!["src".to_string()]);
}

/// A subsystem touched by two distinct authors is never listed as
/// single-author.
#[test]
fn shared_subsystem_is_not_single_author() {
    let db = Database::open_in_memory().expect("open");
    insert_commit(
        &db,
        "a1",
        "Alice",
        "alice@x.com",
        "2026-01-15T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    insert_commit(
        &db,
        "b1",
        "Bob",
        "bob@x.com",
        "2026-01-16T00:00:00Z",
        "repo",
        false,
        &["src/other.rs"],
    );

    let summary = build_authorship_summary(db.connection(), "repo").expect("summary");
    assert_eq!(summary.distinct_authors, 2);
    assert!(summary.single_author_subsystems.is_empty());
    assert_eq!(summary.bus_factor, 1); // 50%-of-touches threshold, 1 of 2 needed
}

/// A repository with no commits at all still produces a (zeroed) summary
/// rather than an error — the caller decides how to render zero data.
#[test]
fn empty_repository_yields_zeroed_summary() {
    let db = Database::open_in_memory().expect("open");
    let summary = build_authorship_summary(db.connection(), "nonexistent").expect("summary");
    assert_eq!(summary.distinct_authors, 0);
    assert_eq!(summary.bus_factor, 0);
    assert!(summary.monthly_trajectory.is_empty());
}

/// The monthly trajectory keeps at most the most recent 12 active months,
/// even when the database spans more than a year of history.
#[test]
fn trajectory_caps_at_twelve_months() {
    let db = Database::open_in_memory().expect("open");
    // 14 distinct months: 2024-11 .. 2025-12.
    let months = [
        "2024-11", "2024-12", "2025-01", "2025-02", "2025-03", "2025-04", "2025-05", "2025-06",
        "2025-07", "2025-08", "2025-09", "2025-10", "2025-11", "2025-12",
    ];
    for (i, month) in months.iter().enumerate() {
        insert_commit(
            &db,
            &format!("c{i}"),
            "Alice",
            "alice@x.com",
            &format!("{month}-01T00:00:00Z"),
            "repo",
            false,
            &["src/lib.rs"],
        );
    }
    let summary = build_authorship_summary(db.connection(), "repo").expect("summary");
    assert_eq!(summary.monthly_trajectory.len(), 12);
    // Oldest-first, and only the most recent 12 survive — 2024-11/12 dropped.
    assert_eq!(summary.monthly_trajectory.first().unwrap().month, "2025-01");
    assert_eq!(summary.monthly_trajectory.last().unwrap().month, "2025-12");
}

/// (#6082) A month's `commits` figure counts commits, not the file touches the
/// `commits JOIN files` query returns one row of per file. Before the fix this
/// read 7 (the row count) for 2 commits, which is how the trusty-tools
/// self-audit reported ~9400 commits in a month that had 824.
#[test]
fn commits_count_commits_not_file_touches() {
    let db = Database::open_in_memory().expect("open");
    insert_commit(
        &db,
        "a1",
        "Alice",
        "alice@x.com",
        "2026-01-15T00:00:00Z",
        "repo",
        false,
        &["src/a.rs", "src/b.rs", "src/c.rs", "docs/d.md"],
    );
    insert_commit(
        &db,
        "b1",
        "Bob",
        "bob@x.com",
        "2026-01-16T00:00:00Z",
        "repo",
        false,
        &["src/e.rs", "src/f.rs", "docs/g.md"],
    );

    let summary = build_authorship_summary(db.connection(), "repo").expect("summary");
    assert_eq!(summary.monthly_trajectory.len(), 1);
    let january = &summary.monthly_trajectory[0];
    assert_eq!(january.month, "2026-01");
    assert_eq!(
        january.commits, 2,
        "2 commits touching 7 files must count 2, not 7"
    );
    assert_eq!(
        january.active_authors, 2,
        "active_authors was already correct and must stay so"
    );
}

/// (#5453) One person committing under two emails is ONE author, because the
/// aggregation joins `commits.author_id → authors.canonical_email` rather than
/// grouping raw commit emails. Against the raw-email grouping this replaced,
/// `distinct_authors` reads 2 and `bus_factor` reads 1 out of a two-author
/// field — the exact understatement of concentration issue #5453 named.
#[test]
fn aliases_collapse_through_the_identity_resolver() {
    let db = Database::open_in_memory().expect("open");
    let alice = insert_author(&db, "Alice", "alice@x.com");
    for (sha, email, path) in [
        ("a1", "alice@x.com", "src/lib.rs"),
        ("a2", "alice@corp.example", "docs/readme.md"),
    ] {
        insert_commit(
            &db,
            sha,
            "Alice",
            email,
            "2026-01-15T00:00:00Z",
            "repo",
            false,
            &[path],
        );
        link_commit(&db, sha, alice);
    }

    let summary = build_authorship_summary(db.connection(), "repo").expect("summary");
    assert_eq!(
        summary.distinct_authors, 1,
        "both aliases resolve to one canonical author"
    );
    assert!(
        (summary.top_author_share_pct - 100.0).abs() < f64::EPSILON,
        "one author holds every touch: {}",
        summary.top_author_share_pct
    );
    assert_eq!(
        summary.unresolved_authors, 0,
        "every commit was linked, so nothing is unresolved"
    );
    assert!(
        !summary.caveats.iter().any(|c| c.contains("never linked")),
        "a fully-resolved run must not carry the unresolved caveat: {:?}",
        summary.caveats
    );
}

/// (#5453) A commit the resolver never linked keeps its raw identity AND is
/// counted into `unresolved_authors`, which then earns its own caveat. This is
/// the honesty gate: the figures are still published, but a reader is told how
/// many identities they under-merge.
#[test]
fn unresolved_authors_are_counted_and_caveated() {
    let db = Database::open_in_memory().expect("open");
    let alice = insert_author(&db, "Alice", "alice@x.com");
    insert_commit(
        &db,
        "a1",
        "Alice",
        "alice@x.com",
        "2026-01-15T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    link_commit(&db, "a1", alice);
    // Never resolved: no `authors` row, so `author_id` stays NULL.
    insert_commit(
        &db,
        "c1",
        "Carol",
        "carol@x.com",
        "2026-01-16T00:00:00Z",
        "repo",
        false,
        &["src/other.rs"],
    );

    let summary = build_authorship_summary(db.connection(), "repo").expect("summary");
    assert_eq!(summary.distinct_authors, 2);
    assert_eq!(
        summary.unresolved_authors, 1,
        "exactly Carol's identity is unresolved"
    );
    let caveat = summary
        .caveats
        .iter()
        .find(|c| c.contains("never linked"))
        .expect("an unresolved run must name the count in a caveat");
    assert!(
        caveat.contains('1'),
        "the caveat must carry the figure: {caveat}"
    );
}

/// (#5453 review) The probe that separates "this name matched nothing" from
/// "this repository is empty" — the distinction `build_authorship_summary`
/// cannot make, since both aggregate zero rows into a confident zero.
#[test]
fn a_name_that_matches_nothing_is_detectable() {
    let db = Database::open_in_memory().expect("open");
    assert!(
        !repository_has_commits(db.connection(), "acme-web").expect("probe"),
        "an empty database matches no name"
    );
    assert!(
        recorded_repository_names(db.connection())
            .expect("names")
            .is_empty(),
        "an empty database records no names"
    );

    insert_commit(
        &db,
        "a1",
        "Alice",
        "alice@x.com",
        "2026-01-15T00:00:00Z",
        "acme_web",
        false,
        &["src/lib.rs"],
    );

    assert!(repository_has_commits(db.connection(), "acme_web").expect("probe"));
    assert!(
        !repository_has_commits(db.connection(), "acme-web").expect("probe"),
        "a name one character off must not match — that is the drift this catches"
    );
    assert_eq!(
        recorded_repository_names(db.connection()).expect("names"),
        vec!["acme_web".to_string()],
        "the recorded name is what the gap line points the operator at"
    );
}
