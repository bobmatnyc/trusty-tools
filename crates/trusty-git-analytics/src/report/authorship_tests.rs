//! Tests for the authorship artifact (#5453, #6004).

use rusqlite::params;

use super::{
    build_authorship_summary, build_authorship_summary_with, merge_suggestions,
    recorded_repository_names, repository_has_commits, AuthorshipSummary,
    AUTHORSHIP_SCHEMA_VERSION,
};
use crate::core::db::Database;
use crate::core::errors::Result;

/// Build one repository's summary, scanning for merge suggestions first.
///
/// Why (#6142 review): the suggestion scan moved OUT of
/// `build_authorship_summary_with` — it is O(n²) in identities and the `authors`
/// table is shared across every repository, so the audit runs it once. This
/// helper restores the one-call shape for tests that do not care which caller
/// supplies the set. `None` for the domain matches an unconfigured project;
/// `the_configured_domain_reaches_the_risk_flag` supplies a real one.
fn summary_for(conn: &rusqlite::Connection, repository: &str) -> Result<AuthorshipSummary> {
    let suggestions = merge_suggestions(conn, None)?;
    build_authorship_summary_with(conn, repository, &suggestions)
}

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

    let summary = summary_for(db.connection(), "repo").expect("summary");
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

    let summary = summary_for(db.connection(), "repo").expect("summary");
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

    let summary = summary_for(db.connection(), "repo").expect("summary");
    assert_eq!(summary.distinct_authors, 2);
    assert!(summary.single_author_subsystems.is_empty());
    assert_eq!(summary.bus_factor, 1); // 50%-of-touches threshold, 1 of 2 needed
}

/// A repository with no commits at all still produces a (zeroed) summary
/// rather than an error — the caller decides how to render zero data.
#[test]
fn empty_repository_yields_zeroed_summary() {
    let db = Database::open_in_memory().expect("open");
    let summary = summary_for(db.connection(), "nonexistent").expect("summary");
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
    let summary = summary_for(db.connection(), "repo").expect("summary");
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

    let summary = summary_for(db.connection(), "repo").expect("summary");
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

    let summary = summary_for(db.connection(), "repo").expect("summary");
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

    let summary = summary_for(db.connection(), "repo").expect("summary");
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

/// Seed one person committing under two identities: `alice@corp.com`, which
/// the collection pass resolved, and `alice@personal.com`, which it did not.
///
/// Why (#6142): this is the corpus shape the issue describes — a contributor
/// whose personal-email commits sit under a second, unlinked identity, so
/// every concentration figure reads lower than reality. Both halves of the
/// fixture (before and after a confirmed merge) start here.
fn seed_split_identity(db: &Database) -> i64 {
    let corp = insert_author(db, "Alice", "alice@corp.com");
    insert_commit(
        db,
        "c1",
        "Alice",
        "alice@corp.com",
        "2026-01-15T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    link_commit(db, "c1", corp);
    insert_commit(
        db,
        "c2",
        "Alice",
        "alice@personal.com",
        "2026-01-16T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    corp
}

/// Record a confirmed merge exactly as `tga aliases merge` does: the source
/// email is appended to the destination's `authors.aliases` JSON array.
fn record_confirmed_alias(db: &Database, canonical_email: &str, alias: &str) {
    let updated = db
        .connection()
        .execute(
            "UPDATE authors SET aliases = ?1 WHERE canonical_email = ?2",
            params![
                serde_json::to_string(&vec![alias]).expect("encode aliases"),
                canonical_email
            ],
        )
        .expect("record alias");
    assert_eq!(updated, 1, "the destination identity must exist");
}

/// (#6142) One person under two identities understates every concentration
/// figure until the merge is confirmed — and the confirmed merge must be
/// applied by the report itself, not left to a separate manual step.
#[test]
fn confirmed_alias_merge_collapses_the_authorship_metric() {
    // Before: no confirmed merge recorded. Alice reads as two authors and
    // her true 100% ownership reads as 50%.
    let before = Database::open_in_memory().expect("open");
    seed_split_identity(&before);
    let split = summary_for(before.connection(), "repo").expect("summary");
    assert_eq!(
        split.distinct_authors, 2,
        "the unmerged corpus must still read as two authors"
    );
    assert!(
        (split.top_author_share_pct - 50.0).abs() < f64::EPSILON,
        "top_author_share_pct must be understated at 50%, got {}",
        split.top_author_share_pct
    );
    assert_eq!(split.unresolved_authors, 1);

    // After: the same corpus with the merge confirmed in `authors.aliases`.
    let after = Database::open_in_memory().expect("open");
    seed_split_identity(&after);
    record_confirmed_alias(&after, "alice@corp.com", "alice@personal.com");
    let merged = summary_for(after.connection(), "repo").expect("summary");
    assert_eq!(
        merged.distinct_authors, 1,
        "a confirmed merge must collapse the two identities"
    );
    assert!(
        (merged.top_author_share_pct - 100.0).abs() < f64::EPSILON,
        "top_author_share_pct must read 100% once merged, got {}",
        merged.top_author_share_pct
    );
    assert_eq!(
        merged.unresolved_authors, 0,
        "an identity folded by a confirmed merge is no longer unresolved"
    );
}

/// (#6142) A HIGH-confidence suggestion nobody has confirmed must raise the
/// risk flag rather than being applied — and the flag must name the metrics,
/// the count, and the command.
#[test]
fn suggested_but_unmerged_identity_raises_a_risk_flag() {
    let db = Database::open_in_memory().expect("open");
    // Two `authors` rows under one display name: the same-name signal scores
    // this pair 0.95, well above the HIGH cutoff. Neither row records the
    // other as a confirmed alias, so the merge is only suggested.
    let corp = insert_author(&db, "Alice", "alice@corp.com");
    let personal = insert_author(&db, "Alice", "alice@personal.com");
    insert_commit(
        &db,
        "s1",
        "Alice",
        "alice@corp.com",
        "2026-01-15T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    link_commit(&db, "s1", corp);
    insert_commit(
        &db,
        "s2",
        "Alice",
        "alice@personal.com",
        "2026-01-16T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    link_commit(&db, "s2", personal);

    let summary = summary_for(db.connection(), "repo").expect("summary");

    // The suggestion must NOT have been applied.
    assert_eq!(
        summary.distinct_authors, 2,
        "an unconfirmed suggestion must never be auto-merged"
    );

    let risk = summary
        .identity_merge_risk
        .as_ref()
        .expect("an unconfirmed HIGH-confidence pair must raise the flag");
    assert_eq!(risk.suggested_unmerged, 1);
    assert_eq!(
        risk.affected_metrics,
        vec!["bus_factor".to_string(), "top_author_share_pct".to_string()],
        "the flag must sit next to the metrics a split identity distorts"
    );
    assert!(
        risk.resolve_command.contains("tga aliases"),
        "the flag must name the resolving command, got {}",
        risk.resolve_command
    );
    assert!(
        summary
            .caveats
            .iter()
            .any(|c| c.contains("suggested for merge")),
        "a caveat-only renderer must still see the flag: {:?}",
        summary.caveats
    );
}

/// (#6142) Confirming the merge clears the flag — the report must not keep
/// warning about work the operator already did.
#[test]
fn a_confirmed_merge_clears_the_risk_flag() {
    let db = Database::open_in_memory().expect("open");
    let corp = insert_author(&db, "Alice", "alice@corp.com");
    insert_commit(
        &db,
        "s1",
        "Alice",
        "alice@corp.com",
        "2026-01-15T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    link_commit(&db, "s1", corp);
    insert_commit(
        &db,
        "s2",
        "Alice",
        "alice@personal.com",
        "2026-01-16T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    // `tga aliases merge` deletes the source row and records the alias; only
    // one `authors` row survives, so no pair remains to suggest.
    record_confirmed_alias(&db, "alice@corp.com", "alice@personal.com");

    let summary = summary_for(db.connection(), "repo").expect("summary");
    assert_eq!(summary.distinct_authors, 1);
    assert!(
        summary.identity_merge_risk.is_none(),
        "a confirmed merge must clear the flag, got {:?}",
        summary.identity_merge_risk
    );
}

/// (#6142 review) A confirmed merge must survive the next collect.
///
/// `upsert_observed_authors` relinks every `author_id IS NULL` commit on every
/// run, and the resolver re-creates a row for any email it does not recognise —
/// including the one `tga aliases merge` deleted. Two things then have to hold:
/// the resolver must route the merged-away email to the identity that absorbed
/// it, and the report must apply the alias map to the resolver's ANSWER, not
/// only to unlinked rows.
#[test]
fn a_confirmed_merge_survives_a_recollect() {
    use crate::collect::identity::resolver::IdentityResolver;

    let db = Database::open_in_memory().expect("open");
    let corp = insert_author(&db, "Alice", "alice@corp.com");
    insert_commit(
        &db,
        "s1",
        "Alice",
        "alice@corp.com",
        "2026-01-15T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    link_commit(&db, "s1", corp);
    insert_commit(
        &db,
        "s2",
        "Alice",
        "alice@personal.com",
        "2026-01-16T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    link_commit(&db, "s2", corp);
    record_confirmed_alias(&db, "alice@corp.com", "alice@personal.com");

    // The next collect observes a NEW commit under the merged-away address.
    // Nothing links it, so the resolver is asked about that address again —
    // exactly what `upsert_observed_authors` does.
    insert_commit(
        &db,
        "s3",
        "Alice",
        "alice@personal.com",
        "2026-01-17T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    let resolver = IdentityResolver::new(None);
    let id = resolver
        .upsert_author(&db, "Alice", "alice@personal.com")
        .expect("upsert");
    assert_eq!(
        id, corp,
        "the merged-away email must route to the identity that absorbed it, \
         not to a re-created source row"
    );
    let rows: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM authors", [], |r| r.get(0))
        .expect("count authors");
    assert_eq!(rows, 1, "the deleted source row must not come back");
    db.connection()
        .execute(
            "UPDATE commits SET author_id = ?1 WHERE author_id IS NULL",
            params![id],
        )
        .expect("relink");

    let summary = summary_for(db.connection(), "repo").expect("summary");
    assert_eq!(
        summary.distinct_authors, 1,
        "a re-collect must not split a confirmed merge back apart"
    );
    assert_eq!(summary.top_author_share_pct.round() as i64, 100);
    assert_eq!(summary.bus_factor, 1);
}

/// (#6142 review) The report path must apply the alias map to the RESOLVER's
/// answer, not only to unlinked commits.
///
/// A database can hold both rows at once — an operator who merged, re-collected
/// against an older tga, and is now reading a report. The alias array is the
/// operator's decision and outranks the row the resolver points at.
#[test]
fn a_merged_email_collapses_even_when_its_row_was_re_created() {
    let db = Database::open_in_memory().expect("open");
    let corp = insert_author(&db, "Alice", "alice@corp.com");
    // The source row the merge deleted, re-created by a later collect.
    let personal = insert_author(&db, "Alice", "alice@personal.com");
    record_confirmed_alias(&db, "alice@corp.com", "alice@personal.com");
    for (sha, email, id) in [
        ("s1", "alice@corp.com", corp),
        ("s2", "alice@personal.com", personal),
    ] {
        insert_commit(
            &db,
            sha,
            "Alice",
            email,
            "2026-01-15T00:00:00Z",
            "repo",
            false,
            &["src/lib.rs"],
        );
        link_commit(&db, sha, id);
    }

    let summary = summary_for(db.connection(), "repo").expect("summary");
    assert_eq!(
        summary.distinct_authors, 1,
        "a linked commit under a confirmed alias must still collapse"
    );
    assert_eq!(summary.top_author_share_pct.round() as i64, 100);
    assert!(
        summary.identity_merge_risk.is_none(),
        "a pair the operator already merged is not unmerged, got {:?}",
        summary.identity_merge_risk
    );
}

/// (#6142) The configured canonical domain must reach the risk flag.
///
/// The `.local`, GitHub-noreply and domain-typo signals issue #6142 names all
/// require it; the report used to hard-code `None` and mute every one of them.
#[test]
fn the_configured_domain_reaches_the_risk_flag() {
    let db = Database::open_in_memory().expect("open");
    // A `.local` hostname address alongside the real one. Only the noise pass
    // pairs these, and only when it knows the canonical domain.
    let corp = insert_author(&db, "Alice Smith", "alice@corp.com");
    let local = insert_author(&db, "A. Smith", "alice@laptop.local");
    for (sha, email, id) in [
        ("s1", "alice@corp.com", corp),
        ("s2", "alice@laptop.local", local),
    ] {
        insert_commit(
            &db,
            sha,
            "Alice",
            email,
            "2026-01-15T00:00:00Z",
            "repo",
            false,
            &["src/lib.rs"],
        );
        link_commit(&db, sha, id);
    }

    let muted = merge_suggestions(db.connection(), None).expect("scan");
    let with_domain = merge_suggestions(db.connection(), Some("corp.com")).expect("scan");
    assert!(
        muted.is_empty(),
        "without the domain the .local signal cannot fire: {muted:?}"
    );
    assert!(
        with_domain.iter().any(|s| s.src == "alice@laptop.local"),
        "the configured domain must surface the .local identity: {with_domain:?}"
    );

    let flagged =
        build_authorship_summary_with(db.connection(), "repo", &with_domain).expect("summary");
    let risk = flagged
        .identity_merge_risk
        .expect("a .local identity beside a top author must raise the flag");
    assert_eq!(risk.suggested_unmerged, 1);
}

/// (#6142) A suggestion touching only long-tail authors must not flag —
/// it cannot move bus factor or top-author share enough to matter.
#[test]
fn a_long_tail_suggestion_does_not_flag() {
    let db = Database::open_in_memory().expect("open");
    // Eleven top authors with two touches each, then a pair sharing a display
    // name with one touch each. Ranking is by touch count, so the suggested
    // pair sits outside the first ten. The names are mutually far apart in
    // edit distance so no signal other than the intended one fires.
    const TAIL_FIXTURE_NAMES: &[&str] = &[
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
        "juliet", "kilo",
    ];
    for name in TAIL_FIXTURE_NAMES {
        let email = format!("{name}@corp.com");
        let id = insert_author(&db, name, &email);
        let sha = format!("t-{name}");
        insert_commit(
            &db,
            &sha,
            name,
            &email,
            "2026-01-15T00:00:00Z",
            "repo",
            false,
            &["src/lib.rs", "src/other.rs"],
        );
        link_commit(&db, &sha, id);
    }
    let z1 = insert_author(&db, "Zed", "zed@corp.com");
    let z2 = insert_author(&db, "Zed", "zed@personal.com");
    insert_commit(
        &db,
        "z1",
        "Zed",
        "zed@corp.com",
        "2026-01-15T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    link_commit(&db, "z1", z1);
    insert_commit(
        &db,
        "z2",
        "Zed",
        "zed@personal.com",
        "2026-01-16T00:00:00Z",
        "repo",
        false,
        &["src/lib.rs"],
    );
    link_commit(&db, "z2", z2);

    let summary = summary_for(db.connection(), "repo").expect("summary");
    assert_eq!(summary.distinct_authors, 13);
    assert!(
        summary.identity_merge_risk.is_none(),
        "a pair outside the top-N must not raise the flag, got {:?}",
        summary.identity_merge_risk
    );
}

/// (#6142 review) The two-argument `build_authorship_summary` is the signature
/// every caller before the suggestion set existed compiled against, so it stays
/// callable: it scans for its own suggestions and answers identically to
/// `build_authorship_summary_with` given that same scan.
#[test]
fn the_wrapper_scans_its_own_suggestions() {
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
        "Alice",
        "alice@example.com",
        "2026-02-15T00:00:00Z",
        "repo",
        false,
        &["src/other.rs"],
    );

    let scanned = merge_suggestions(db.connection(), None).expect("scan");
    let delegated =
        build_authorship_summary_with(db.connection(), "repo", &scanned).expect("summary");
    let wrapped = build_authorship_summary(db.connection(), "repo").expect("summary");

    assert_eq!(
        wrapped.to_json().expect("json"),
        delegated.to_json().expect("json"),
        "the wrapper must answer exactly as the delegating form does"
    );
}
