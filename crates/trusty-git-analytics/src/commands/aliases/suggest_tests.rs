//! CLI-level tests for `tga aliases suggest`.
//!
//! Detection-algorithm coverage lives with the algorithm, in
//! `tga::collect::identity::suggest::tests` (#6142). What remains here is the
//! handler's own behaviour: threading the configured canonical domain through,
//! and the `--auto-accept` merge.

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
    run(&cfg, &mut db, 0.85, true).expect("run");

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
    run(&cfg, &mut db, 0.85, false).expect("run");

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
    run(&cfg, &mut db, 0.5, false).expect("run");
}
