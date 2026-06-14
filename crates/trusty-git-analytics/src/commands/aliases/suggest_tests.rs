use super::*;
use crate::commands::aliases::tests::{insert_author, insert_commit};
use tga::core::config::TeamConfig;

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
fn same_name_different_email_detected() {
    let db = Database::open_in_memory().expect("open");
    insert_author(&db, "Bob Matsuoka", "bob@matsuoka.com");
    insert_author(&db, "Bob Matsuoka", "robert.matsuoka@duettoresearch.com");
    let out = detect_same_name_pairs(&db).expect("detect");
    assert_eq!(out.len(), 1, "exactly one pair expected, got {out:?}");
    assert!(out[0].confidence >= 0.9);
    assert!(out[0].reason.contains("same canonical_name"));
}

#[test]
fn edit_distance_local_part_detected() {
    let db = Database::open_in_memory().expect("open");
    insert_author(&db, "Alice", "alice@example.com");
    // `aliace` is edit-distance 2 from `alice` (insert + transpose),
    // but actually it's distance 1 (one insert). Make distance exactly 1.
    insert_author(&db, "Other", "alicea@example.com");
    let out = detect_edit_distance_pairs(&db).expect("detect");
    assert!(
        out.iter().any(|s| s.reason.contains("edit-distance")),
        "expected an edit-distance suggestion, got {out:?}"
    );
}

#[test]
fn dotlocal_email_routed_to_canonical_domain() {
    let db = Database::open_in_memory().expect("open");
    insert_author(&db, "Bob", "bob@HOST.local");
    insert_author(&db, "Bob", "bob@duettoresearch.com");
    let out = detect_noise_patterns(&db, Some("duettoresearch.com")).expect("detect");
    assert!(
        out.iter().any(|s| s.reason.contains(".local hostname")),
        "expected .local suggestion, got {out:?}"
    );
}

#[test]
fn github_noreply_routed_to_login() {
    let db = Database::open_in_memory().expect("open");
    insert_author(
        &db,
        "A",
        "129991831+andreramosduetto@users.noreply.github.com",
    );
    insert_author(&db, "B", "andreramosduetto@duettoresearch.com");
    let out = detect_noise_patterns(&db, Some("duettoresearch.com")).expect("detect");
    assert!(
        out.iter().any(|s| s.reason.contains("GitHub noreply")),
        "expected github noreply suggestion, got {out:?}"
    );
}

#[test]
fn domain_typo_detected_against_canonical_domain() {
    let db = Database::open_in_memory().expect("open");
    insert_author(&db, "Carol", "carol@duettoresearh.com"); // typo
    insert_author(&db, "Carol", "carol@duettoresearch.com");
    let out = detect_noise_patterns(&db, Some("duettoresearch.com")).expect("detect");
    assert!(
        out.iter().any(|s| s.reason.contains("domain typo")),
        "expected domain-typo suggestion, got {out:?}"
    );
}

#[test]
fn same_sha_two_emails_detected() {
    // Why: the production schema enforces UNIQUE on commits.sha so we
    // cannot directly stage two rows with the same SHA against the
    // standard `Database`. The detector only reads `(sha, author_email)`,
    // so we exercise it against a fresh in-memory connection where we
    // own a stripped-down commits table without the UNIQUE constraint.
    // This mirrors the data shape the query actually consumes.
    let db = Database::open_in_memory().expect("open");
    // Replace the commits table with one that lacks UNIQUE on sha.
    // We must also drop the FK-bearing children (fact_commit_reachability,
    // fact_commit_effort, etc.) first because SQLite refuses to drop a
    // table while another table references it via FK.
    let conn = db.connection();
    conn.execute("PRAGMA foreign_keys = OFF", [])
        .expect("fk off");
    // Discover and drop all tables that reference `commits` via FK so
    // the `DROP TABLE commits` is unconstrained.
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .expect("prepare");
    let names: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("rows")
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);
    for n in &names {
        if n != "authors" && n != "sqlite_sequence" {
            let _ = conn.execute(&format!("DROP TABLE IF EXISTS \"{n}\""), []);
        }
    }
    conn.execute(
        "CREATE TABLE commits (sha TEXT, author_id INTEGER, author_name TEXT, \
         author_email TEXT, timestamp TEXT, message TEXT, repository TEXT)",
        [],
    )
    .expect("recreate commits");

    let a = insert_author(&db, "A", "a@example.com");
    let b = insert_author(&db, "B", "b@example.com");
    conn.execute(
        "INSERT INTO commits (sha, author_id, author_name, author_email, timestamp, \
         message, repository) VALUES \
         ('shared-sha', ?1, 'A', 'a@example.com', '2024-01-01T00:00:00Z', 'm', 'r'),\
         ('shared-sha', ?2, 'B', 'b@example.com', '2024-01-01T00:00:00Z', 'm', 'r')",
        params![a, b],
    )
    .expect("insert");

    let out = detect_commit_sha_cooccurrence(&db).expect("detect");
    assert!(
        out.iter().any(|s| s.reason.contains("same SHA")),
        "expected commit-SHA co-occurrence, got {out:?}"
    );
}

#[test]
fn dedupe_keeps_highest_confidence() {
    let input = vec![
        Suggestion {
            src: "x".into(),
            dst: "y".into(),
            confidence: 0.7,
            reason: "weak".into(),
        },
        Suggestion {
            src: "x".into(),
            dst: "y".into(),
            confidence: 0.95,
            reason: "strong".into(),
        },
    ];
    let out = dedupe_and_rank(input, 0.5);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].reason, "strong");
    assert!((out[0].confidence - 0.95).abs() < 1e-9);
}

#[test]
fn confidence_floor_filters() {
    let input = vec![
        Suggestion {
            src: "a".into(),
            dst: "b".into(),
            confidence: 0.6,
            reason: "weak".into(),
        },
        Suggestion {
            src: "c".into(),
            dst: "d".into(),
            confidence: 0.95,
            reason: "strong".into(),
        },
    ];
    let out = dedupe_and_rank(input, 0.85);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].src, "c");
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

#[test]
fn split_email_basic() {
    assert_eq!(
        split_email("Bob@Example.COM"),
        Some(("bob".to_string(), "example.com".to_string()))
    );
    assert_eq!(split_email("no-at"), None);
    assert_eq!(split_email("@nolocal.com"), None);
    assert_eq!(split_email("local@"), None);
}
