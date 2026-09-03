//! Tests for `tga inspect` — schema reading, the attestation, the fail-open
//! arms, and the enforced `diff_for_commit` caller check (#5218).

use std::path::{Path, PathBuf};

use crate::core::db::Database;
use crate::core::errors::TgaError;

use super::attest::{attest, DIFF_API_SITES, DIFF_TEXT_CONSUMERS, NO_CONTENT_CLAIM};
use super::schema::{snapshot, ObjectKind};
use super::text_columns::{classify, TextClass, CONSTRAINED, EMBEDDED_PAYLOAD, FREE_TEXT};
use super::{open_read_only, render};

// ─── Fixtures ─────────────────────────────────────────────────────────────────

/// A temp directory removed on drop, so nothing this module writes survives it.
struct TempDir {
    path: PathBuf,
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn temp_dir(tag: &str) -> TempDir {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "tga-inspect-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir_all(&path).expect("mkdir");
    TempDir { path }
}

/// A fully-migrated on-disk database at `<dir>/tga.db`.
fn migrated_db(dir: &TempDir) -> PathBuf {
    let path = dir.path.join("tga.db");
    Database::open(&path).expect("open and migrate");
    path
}

// ─── Fail-open arms ───────────────────────────────────────────────────────────

/// Why: `Database::open` CREATES a missing file and migrates it, so an
/// inspection built on that path would print a complete empty schema and exit
/// 0 — telling a reviewer their unreadable database holds nothing.
/// What: asserts the missing-path arm errors and that the message names the
/// path rather than reporting an empty success.
/// Test: this test itself.
#[test]
fn open_read_only_names_a_missing_database() {
    let dir = temp_dir("missing");
    let missing = dir.path.join("absent.db");

    let err = open_read_only(&missing).expect_err("a missing database must not open");
    assert!(
        matches!(err, TgaError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
    assert!(
        err.to_string().contains("absent.db"),
        "the error must name the path it could not find: {err}"
    );
    assert!(
        !missing.exists(),
        "inspecting a missing database must not create one"
    );
}

/// Why: `--database` pointed at a directory is the same fail-open hazard with a
/// different cause, and SQLite's own message for it names no path.
/// Test: this test itself.
#[test]
fn open_read_only_names_a_directory() {
    let dir = temp_dir("isdir");

    let err = open_read_only(&dir.path).expect_err("a directory must not open");
    assert!(
        matches!(err, TgaError::ValidationError(_)),
        "expected ValidationError, got {err:?}"
    );
    assert!(
        err.to_string().contains("is a directory"),
        "the error must name the cause: {err}"
    );
}

/// Why: a truncated or half-written file opens fine under SQLite's lazy header
/// check and only fails on the first query, which is the wrong place to learn
/// the file is not a database.
/// Test: this test itself.
#[test]
fn open_read_only_names_a_non_sqlite_file() {
    let dir = temp_dir("notdb");
    let path = dir.path.join("notes.txt");
    std::fs::write(&path, b"this is not a database").expect("write");

    let err = open_read_only(&path).expect_err("a text file must not open as a database");
    assert!(
        matches!(err, TgaError::ValidationError(_)),
        "expected ValidationError, got {err:?}"
    );
    assert!(
        err.to_string().contains("not a SQLite database"),
        "the error must name the cause: {err}"
    );
}

/// Why: an inspection that migrated the operator's database would report the
/// schema it just created rather than the one it was handed.
/// What: opens a v0 database (an empty but valid SQLite file), snapshots it, and
/// asserts no tga table appeared.
/// Test: this test itself.
#[test]
fn open_read_only_does_not_migrate() {
    let dir = temp_dir("nomigrate");
    let path = dir.path.join("empty.db");
    rusqlite::Connection::open(&path)
        .expect("create")
        .execute_batch("CREATE TABLE marker (x INTEGER);")
        .expect("seed");

    let conn = open_read_only(&path).expect("open the valid, un-migrated file");
    let snap = snapshot(&conn).expect("snapshot");

    assert_eq!(
        snap.schema_version, None,
        "no schema_migrations table exists"
    );
    let names: Vec<&str> = snap.objects.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, vec!["marker"], "inspection must not create tables");

    // A write through the read-only handle must be refused.
    let write = conn.execute_batch("CREATE TABLE sneaky (y INTEGER);");
    assert!(write.is_err(), "the connection must be read-only");
}

// ─── Schema snapshot ──────────────────────────────────────────────────────────

/// Why: the command's whole claim is that it shows what the database actually
/// holds, so the reader must return every table and every column, not a
/// hand-maintained subset.
/// What: snapshots a fully-migrated database and checks a table from the first
/// migration, one from the newest, a view, and a column added by an ALTER.
/// Test: this test itself.
#[test]
fn snapshot_reads_every_table_and_column() {
    let dir = temp_dir("snapshot");
    let path = migrated_db(&dir);
    let conn = open_read_only(&path).expect("open");
    let snap = snapshot(&conn).expect("snapshot");

    let names: Vec<&str> = snap.objects.iter().map(|o| o.name.as_str()).collect();
    for expected in [
        "commits",
        "files",
        "work_items",
        "fact_pm_effort",
        "schema_migrations",
        "v_lead_time",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }

    let commits = snap
        .objects
        .iter()
        .find(|o| o.name == "commits")
        .expect("commits table");
    assert_eq!(commits.kind, ObjectKind::Table);
    let cols: Vec<&str> = commits.columns.iter().map(|c| c.name.as_str()).collect();
    assert!(cols.contains(&"message"), "column from 0001 missing");
    assert!(cols.contains(&"agentic_mode"), "column from 0021 missing");

    let view = snap
        .objects
        .iter()
        .find(|o| o.name == "v_lead_time")
        .expect("view");
    assert_eq!(view.kind, ObjectKind::View);
    assert_eq!(view.row_count, None, "views carry no row count");
}

/// Why: a reviewer reads the row counts to see which tables were actually
/// populated, so a count that ignores rows is worse than none.
/// Test: this test itself.
#[test]
fn snapshot_reports_row_counts() {
    let dir = temp_dir("counts");
    let path = migrated_db(&dir);
    {
        let db = Database::open(&path).expect("reopen");
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
                 VALUES ('abc123', 'Ada', 'ada@example.com', '2026-01-01T00:00:00Z', 'feat: x', 'r')",
                [],
            )
            .expect("insert");
    }

    let conn = open_read_only(&path).expect("open");
    let snap = snapshot(&conn).expect("snapshot");
    let commits = snap
        .objects
        .iter()
        .find(|o| o.name == "commits")
        .expect("commits");
    assert_eq!(commits.row_count, Some(1));
}

// ─── Text-column classification ───────────────────────────────────────────────

/// Why: an inventory of free-text columns written once goes stale the next time
/// a migration adds a `TEXT` column, and the attestation would keep printing a
/// list that no longer covers the schema. This is the check that makes the
/// inventory a standing guard rather than a one-time read.
/// What: snapshots a fully-migrated database and asserts no `TEXT` column
/// classifies as [`TextClass::Unclassified`].
/// Test: this test itself.
#[test]
fn every_text_column_is_classified() {
    let dir = temp_dir("classified");
    let path = migrated_db(&dir);
    let conn = open_read_only(&path).expect("open");
    let snap = snapshot(&conn).expect("snapshot");

    let unclassified: Vec<String> = snap
        .text_columns()
        .into_iter()
        .filter(|(_, c)| c.text_class == Some(TextClass::Unclassified))
        .map(|(t, c)| format!("{}.{}", t.name, c.name))
        .collect();
    assert!(
        unclassified.is_empty(),
        "these TEXT columns are in the schema but in none of FREE_TEXT / \
         EMBEDDED_PAYLOAD / CONSTRAINED — classify them in \
         core::inspect::text_columns: {unclassified:?}"
    );

    // The reverse direction: an inventory entry naming a column that no longer
    // exists is equally misleading in the printed report.
    let live: Vec<String> = snap
        .text_columns()
        .into_iter()
        .map(|(t, c)| format!("{}.{}", t.name, c.name))
        .collect();
    let mut stale: Vec<&str> = Vec::new();
    for key in FREE_TEXT.iter().chain(EMBEDDED_PAYLOAD).chain(CONSTRAINED) {
        if !live.iter().any(|l| l == key) {
            stale.push(key);
        }
    }
    assert!(
        stale.is_empty(),
        "these inventory entries name columns the schema no longer has: {stale:?}"
    );
}

/// Why: a column in two lists would classify by list order rather than by
/// intent, and the one that lost would silently stop being scanned.
/// Test: this test itself.
#[test]
fn text_class_lists_are_disjoint() {
    for key in FREE_TEXT {
        assert!(!EMBEDDED_PAYLOAD.contains(key), "{key} in two lists");
        assert!(!CONSTRAINED.contains(key), "{key} in two lists");
    }
    for key in EMBEDDED_PAYLOAD {
        assert!(!CONSTRAINED.contains(key), "{key} in two lists");
    }
    assert_eq!(
        classify("commits", "message"),
        TextClass::FreeText,
        "the highest-exposure column must classify as free text"
    );
    assert_eq!(
        classify("work_items", "raw_json"),
        TextClass::EmbeddedPayload
    );
    assert_eq!(classify("commits", "sha"), TextClass::Constrained);
    assert_eq!(classify("nope", "nope"), TextClass::Unclassified);
}

// ─── Attestation ──────────────────────────────────────────────────────────────

/// Why: "no code" is the paraphrase #5218 exists to prevent, and a claim string
/// is exactly the kind of text a later edit loosens without noticing.
/// Test: this test itself.
#[test]
fn claim_never_says_no_code() {
    assert!(
        !NO_CONTENT_CLAIM.to_lowercase().contains("no code"),
        "the claim must never assert the database contains no code: {NO_CONTENT_CLAIM}"
    );
    for term in ["file content", "diffs", "patches", "hunks", "blobs"] {
        assert!(
            NO_CONTENT_CLAIM.contains(term),
            "the claim must name {term}: {NO_CONTENT_CLAIM}"
        );
    }
}

/// Why: the pass case has to be reachable, or the verdict carries no
/// information.
/// What: attests a freshly migrated, empty database and asserts no
/// content-bearing column, a populated scan list, and a consistent verdict.
/// Test: this test itself.
#[test]
fn attest_on_a_fresh_database_is_consistent() {
    let dir = temp_dir("attest-clean");
    let path = migrated_db(&dir);
    let conn = open_read_only(&path).expect("open");
    let snap = snapshot(&conn).expect("snapshot");
    let report = attest(&conn, &snap).expect("attest");

    assert!(
        report.content_columns.is_empty(),
        "tga's schema must hold no content-bearing column: {:?}",
        report.content_columns
    );
    assert_eq!(report.verdict, super::attest::Verdict::Consistent);
    assert!(
        report
            .scanned_columns
            .iter()
            .any(|s| s.table == "commits" && s.column == "message"),
        "commits.message must be scanned"
    );
    assert!(
        report
            .scanned_columns
            .iter()
            .any(|s| s.table == "work_items" && s.column == "raw_json"),
        "work_items.raw_json must be scanned at runtime, not read off the migration"
    );
    assert!(
        report
            .scanned_columns
            .iter()
            .all(|s| !matches!(s.class, TextClass::Constrained)),
        "constrained columns must not be scanned"
    );
}

/// Why: a scan that cannot find a diff proves nothing about the databases where
/// one exists — the failing arm is what makes the passing arm evidence.
/// What: pastes a raw unified diff into `commits.message` and a
/// `serde_json`-serialized one into `work_items.raw_json`, then asserts BOTH
/// are counted and the verdict flips. The `raw_json` half is what the JSON
/// escaping fix turns from 0 to 1 — the value reaching SQLite there carries the
/// two-character escape `\n`, not a newline byte, so a newline-anchored
/// predicate matches nothing.
/// Test: this test itself.
#[test]
fn attest_flags_a_diff_pasted_into_a_commit_message() {
    let dir = temp_dir("attest-dirty");
    let path = migrated_db(&dir);
    // Escaped by a real serializer, so the stored bytes are whatever the
    // collector would actually have written — never a hand-built literal that
    // could accidentally carry a real newline and pass for the wrong reason.
    let payload = serde_json::to_string(&serde_json::json!({
        "description": "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n",
    }))
    .expect("serialize");
    assert!(
        !payload.contains('\n'),
        "the fixture must carry the JSON escape, not a newline byte: {payload}"
    );

    {
        let db = Database::open(&path).expect("reopen");
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
                 VALUES ('deadbee', 'Ada', 'ada@example.com', '2026-01-01T00:00:00Z', ?1, 'r')",
                ["fix: paste\n\ndiff --git a/src/lib.rs b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"],
            )
            .expect("insert commit");
        db.connection()
            .execute(
                "INSERT INTO work_items (id, source, title, status, item_type, raw_json) \
                 VALUES ('W-1', 'jira', 'T', 'Done', 'Bug', ?1)",
                [&payload],
            )
            .expect("insert work item");
    }

    let conn = open_read_only(&path).expect("open");
    let snap = snapshot(&conn).expect("snapshot");
    let report = attest(&conn, &snap).expect("attest");

    let message = report
        .scanned_columns
        .iter()
        .find(|s| s.table == "commits" && s.column == "message")
        .expect("commits.message scan");
    assert_eq!(
        message.diff_shaped_rows, 1,
        "a pasted unified diff must be counted"
    );
    assert_eq!(message.populated, 1);
    assert!(message.max_len > 0);

    let raw_json = report
        .scanned_columns
        .iter()
        .find(|s| s.table == "work_items" && s.column == "raw_json")
        .expect("work_items.raw_json scan");
    assert_eq!(raw_json.populated, 1);
    assert_eq!(
        raw_json.diff_shaped_rows, 1,
        "a diff serialized into JSON must be counted — its newlines are the \
         two-character escape, so the scan has to normalise before matching"
    );

    assert_eq!(report.verdict, super::attest::Verdict::Findings);
}

/// Why: the probe anchors four of its five markers on a newline BYTE, and the
/// escaped forms that carry a diff into a text column have none. Each row below
/// is a way a diff has actually reached a column, plus the prose that must NOT
/// match — without the last one, normalising could be made to pass by widening
/// the markers until everything matches.
/// What: writes each fixture into `commits.message`, attests, and asserts the
/// per-row verdict.
/// Test: this test itself.
#[test]
fn diff_probe_normalises_json_escaped_newlines() {
    let cases: &[(&str, &str, i64)] = &[
        ("raw", "fix\n\ndiff --git a/x b/x\n@@ -1 +1 @@\n-a\n+b\n", 1),
        // JSON escaping: the byte sequence backslash-n, not a newline.
        (
            "json_lf",
            r#"{"description":"--- a/x\n+++ b/x\n@@ -1 +1 @@"}"#,
            1,
        ),
        // A Windows-authored diff serialized the same way.
        (
            "json_crlf",
            r#"{"description":"--- a/x\r\n+++ b/x\r\n@@ -1 +1 @@"}"#,
            1,
        ),
        // Prose that name-drops the markers mid-line must stay uncounted.
        (
            "prose",
            "Reviewed the @@ hunk headers and the --- a/ prefix in passing",
            0,
        ),
    ];

    for (label, message, expected) in cases {
        let dir = temp_dir(&format!("probe-{label}"));
        let path = migrated_db(&dir);
        {
            let db = Database::open(&path).expect("reopen");
            db.connection()
                .execute(
                    "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
                     VALUES ('s1', 'Ada', 'ada@example.com', '2026-01-01T00:00:00Z', ?1, 'r')",
                    [message],
                )
                .expect("insert");
        }
        let conn = open_read_only(&path).expect("open");
        let snap = snapshot(&conn).expect("snapshot");
        let report = attest(&conn, &snap).expect("attest");
        let scan = report
            .scanned_columns
            .iter()
            .find(|s| s.table == "commits" && s.column == "message")
            .expect("commits.message scan");
        assert_eq!(
            scan.diff_shaped_rows, *expected,
            "case {label}: expected {expected} diff-shaped row(s) for {message:?}"
        );
    }
}

// ─── The enforced diff_for_commit caller check ────────────────────────────────

/// Why: #5218's standing guard. `diff_for_commit` computes a real unified diff,
/// so a caller that persisted its result would break [`NO_CONTENT_CLAIM`]
/// without changing one line of SQL. A schema read stays green through that; a
/// list written once goes stale through it. This re-derives the caller set from
/// the source tree on every run.
/// What: walks `src/`, collects every non-comment, non-test line naming
/// `diff_for_commit`, drops the defining and re-exporting files, and asserts the
/// remaining files are exactly [`DIFF_TEXT_CONSUMERS`].
/// Test: this test itself.
#[test]
fn diff_for_commit_callers_match_the_attestation() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut found: Vec<String> = Vec::new();
    collect_callers(&root.join("src"), &root, &mut found);
    found.sort();
    found.dedup();

    let mut attested: Vec<String> = DIFF_TEXT_CONSUMERS
        .iter()
        .map(|c| c.source_path.to_string())
        .collect();
    attested.sort();

    assert_eq!(
        found, attested,
        "the non-test callers of diff_for_commit changed. Update DIFF_TEXT_CONSUMERS in \
         core::inspect::attest, and confirm the new caller does not write diff text to the \
         database — see #5218."
    );
}

/// Walk `dir`, appending the crate-relative path of every file that calls
/// `diff_for_commit` outside a comment and outside test code.
fn collect_callers(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir).expect("read src dir");
    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_callers(&path, root, out);
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .expect("path under crate root")
            .to_string_lossy()
            .replace('\\', "/");
        if DIFF_API_SITES.contains(&rel.as_str()) || is_test_file(&rel) {
            continue;
        }
        let body = std::fs::read_to_string(&path).expect("read source file");
        if body.lines().any(is_caller_line) {
            out.push(rel);
        }
    }
}

/// Whether a crate-relative path is test-only by this project's conventions.
fn is_test_file(rel: &str) -> bool {
    rel.contains("/tests/")
        || rel.ends_with("/tests.rs")
        || rel.ends_with("_test.rs")
        || rel.ends_with("_tests.rs")
}

/// Whether one source line calls `diff_for_commit` rather than mentioning it.
///
/// A comment and a string literal both name the symbol without calling it —
/// this module's own report text is one of each — so both are removed before
/// the match. Everything that survives is code.
fn is_caller_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return false;
    }
    strip_string_literals(line).contains("diff_for_commit")
}

/// Blank out every double-quoted span in `line`.
///
/// Deliberately naive: it does not track escapes or raw strings, because the
/// only thing it must not do is hide real code, and a mishandled escape leaves
/// MORE of the line visible, not less.
fn strip_string_literals(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_string = false;
    for ch in line.chars() {
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if !in_string {
            out.push(ch);
        }
    }
    out
}

// ─── Rendering ────────────────────────────────────────────────────────────────

/// Why: the closure condition is that a reviewer SEES the free-text columns
/// called out, which the data structure alone does not deliver.
/// Test: this test itself.
#[test]
fn schema_report_marks_free_text_columns() {
    let dir = temp_dir("render-schema");
    let path = migrated_db(&dir);
    let conn = open_read_only(&path).expect("open");
    let snap = snapshot(&conn).expect("snapshot");
    let text = render::schema_report(&snap);

    assert!(text.contains("TABLE commits"), "tables must be listed");
    assert!(text.contains("VIEW  v_lead_time"), "views must be listed");
    assert!(
        text.contains("message") && text.contains("← FREE TEXT"),
        "free-text columns must be marked"
    );
    assert!(
        text.contains("← EMBEDDED PAYLOAD"),
        "work_items.raw_json must be marked"
    );
    assert!(
        text.contains("Free-text and payload columns"),
        "the trailing summary must be present"
    );
}

/// Why: DOC-67 §10 quotes this output, so the claim and its caveat must both
/// survive into the rendered text — the claim alone is the overreach.
/// Test: this test itself.
#[test]
fn attestation_report_states_the_claim_and_the_caveat() {
    let dir = temp_dir("render-attest");
    let path = migrated_db(&dir);
    let conn = open_read_only(&path).expect("open");
    let snap = snapshot(&conn).expect("snapshot");
    let report = attest(&conn, &snap).expect("attest");
    let text = render::attestation_report(&report);

    assert!(text.contains(NO_CONTENT_CLAIM));
    assert!(text.contains("not a claim that the database contains no code"));
    assert!(
        text.lines()
            .next()
            .is_some_and(|first| first == NO_CONTENT_CLAIM),
        "the claim must lead the report, so a reader quoting the first line quotes it"
    );
    assert!(text.contains("commits.message"));
    assert!(text.contains("src/profile/diff_sampler/sampler.rs"));
    assert!(text.contains("VERDICT: consistent"));
}
