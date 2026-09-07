//! CLI-level tests for `tga aliases suggest`.
//!
//! Detection-algorithm coverage lives with the algorithm, in
//! `tga::collect::identity::suggest::tests` (#6142). What remains here is the
//! handler's own behaviour: threading the configured canonical domain through,
//! the `--auto-accept` merge, and the `--review-file` near-miss artifact
//! (#6993).

use super::*;
use rusqlite::params;
use tga::core::config::TeamConfig;

use crate::commands::aliases::tests::{insert_author, insert_commit};

fn config_with_domain(domain: &str) -> Config {
    Config {
        team: Some(TeamConfig {
            members: vec![],
            aliases: std::collections::HashMap::new(),
            canonical_domain: Some(domain.to_string()),
        }),
        ..Config::default()
    }
}

/// Two identities that score 0.95 (same canonical_name) plus two that score
/// 0.78 (edit-distance 2 on the local-part), so a 0.85 threshold splits them
/// one to stdout and one to the review file.
fn db_with_one_hit_and_one_near_miss() -> Database {
    let db = Database::open_in_memory().expect("open");
    insert_author(&db, "Bob", "alt@example.com");
    insert_author(&db, "Bob", "bob@example.com");
    insert_author(&db, "Carol", "carol@example.com");
    insert_author(&db, "Carolyn", "carolyn@example.com");
    db
}

#[test]
fn auto_accept_only_merges_high() {
    let mut db = Database::open_in_memory().expect("open");
    // Two identities with the same canonical_name → produces a HIGH
    // (0.95) suggestion. The same-name signal sorts emails
    // alphabetically and uses the lexicographically smaller one as
    // destination, so `alt@example.com` becomes dst and
    // `bob@example.com` becomes src and is removed.
    let alt = insert_author(&db, "Bob", "alt@example.com");
    let bob = insert_author(&db, "Bob", "bob@example.com");
    insert_commit(&db, "sha-bob", bob);
    insert_commit(&db, "sha-alt", alt);

    let cfg = Config::default();
    run(&cfg, &mut db, 0.85, true, None).expect("run");

    // After auto-accept the src (bob@) row should be gone.
    let bob_exists: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM authors WHERE canonical_email = 'bob@example.com'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(
        bob_exists, 0,
        "auto-accept should have removed bob@example.com"
    );
    // Both commits should be attached to the surviving dst row.
    let n: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM commits WHERE author_id = ?1",
            params![alt],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(n, 2);
}

#[test]
fn suggest_without_auto_accept_never_merges() {
    // #6142: a suggestion is not a merge. The report's risk flag depends on
    // an unconfirmed pair staying unconfirmed until a human accepts it.
    let mut db = Database::open_in_memory().expect("open");
    insert_author(&db, "Bob", "alt@example.com");
    insert_author(&db, "Bob", "bob@example.com");

    let cfg = Config::default();
    run(&cfg, &mut db, 0.85, false, None).expect("run");

    let rows: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM authors", [], |r| r.get(0))
        .expect("count");
    assert_eq!(rows, 2, "both identities must survive a suggest-only run");
}

#[test]
fn config_canonical_domain_threads_through() {
    // Smoke test: with a configured canonical_domain, the suggester
    // produces a domain-typo suggestion when the corpus contains one.
    let mut db = Database::open_in_memory().expect("open");
    insert_author(&db, "Z", "z@duettoresearh.com");
    insert_author(&db, "Z", "z@duettoresearch.com");
    let cfg = config_with_domain("duettoresearch.com");
    // Run with auto_accept=false so we don't mutate; just ensure no panic.
    run(&cfg, &mut db, 0.5, false, None).expect("run");
}

#[test]
fn the_review_file_lists_only_near_miss_pairs() {
    // #6993: the pair below `--confidence` reaches the file and nothing else
    // does, and the pair above it still reaches stdout and only stdout.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested").join("identity-review.tsv");
    let mut db = db_with_one_hit_and_one_near_miss();

    let mut stdout: Vec<u8> = Vec::new();
    run_to(
        &Config::default(),
        &mut db,
        0.85,
        false,
        Some(&path),
        &mut stdout,
    )
    .expect("run");

    let written = std::fs::read_to_string(&path).expect("review file");
    let lines: Vec<&str> = written.lines().collect();
    assert_eq!(
        lines[0], "src\tdst\treason\tconfidence\tconfirmed",
        "header names the columns #6993 asks for"
    );
    assert_eq!(
        lines.len(),
        2,
        "exactly one near-miss row belongs in the file, got: {written}"
    );
    assert_eq!(
        lines[1], "carolyn@example.com\tcarol@example.com\tedit-distance 2 on local-part\t0.78\tno",
        "the near-miss row carries both identities, the reason, the score, and confirmed=no"
    );
    assert!(
        !written.contains("bob@example.com"),
        "the above-threshold pair must not leak into the review file: {written}"
    );

    let printed = String::from_utf8(stdout).expect("utf8");
    assert!(
        printed.contains("bob@example.com → alt@example.com"),
        "the above-threshold pair is still printed: {printed}"
    );
    assert!(
        !printed.contains("carolyn@example.com → carol@example.com"),
        "a near miss must not be printed as a suggestion: {printed}"
    );
}

#[test]
fn no_review_file_is_written_without_the_flag() {
    // #6993: the artifact is opt-in — an unflagged run leaves no file behind.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("identity-review.tsv");
    let mut db = db_with_one_hit_and_one_near_miss();

    let mut stdout: Vec<u8> = Vec::new();
    run_to(&Config::default(), &mut db, 0.85, false, None, &mut stdout).expect("run");

    assert!(
        !path.exists(),
        "no review file may be written without --review-file"
    );
    let printed = String::from_utf8(stdout).expect("utf8");
    assert!(
        !printed.contains("near-miss"),
        "an unflagged run says nothing about a review file: {printed}"
    );
}

#[test]
fn an_empty_near_miss_set_still_writes_the_header() {
    // #6993: "no near misses" is an answer the operator asked for, so the
    // flagged run leaves a header-only file rather than no file at all.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("identity-review.tsv");
    let mut db = Database::open_in_memory().expect("open");
    insert_author(&db, "Bob", "alt@example.com");
    insert_author(&db, "Bob", "bob@example.com");

    let mut stdout: Vec<u8> = Vec::new();
    run_to(
        &Config::default(),
        &mut db,
        0.85,
        false,
        Some(&path),
        &mut stdout,
    )
    .expect("run");

    let written = std::fs::read_to_string(&path).expect("review file");
    assert_eq!(written, format!("{REVIEW_HEADER}\n"));
}
