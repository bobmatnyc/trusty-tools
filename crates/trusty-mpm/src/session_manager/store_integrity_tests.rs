//! Tests for session-store corruption detection, disclosure, and repair
//! (#5007).
//!
//! Why: the 2026-08-06 incident was a complete JSON document followed by 1111
//! stale bytes from a longer earlier version of the same document. Every test
//! that claims to cover that failure is built on a fixture with exactly that
//! shape — [`corrupt_with_stale_tail`] — rather than on generic garbage like
//! `{ not json ]`, which any parse failure would produce and which therefore
//! proves nothing about this defect.
//! What: detection (`load_*`), the writer race (`two_stores_*`), disclosure
//! (`store_records_*`, `log_reload_fallback_*`), and repair (`repair_*`,
//! `plan_*`), plus unit coverage of every helper in `store_integrity.rs`.
//! Test: this file IS the test module; run with
//! `cargo test -p trusty-mpm store_integrity`.

use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use tempfile::TempDir;

use super::*;
use crate::session_manager::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use crate::session_manager::store::{SessionStore, StoreError};

/// A record with a caller-chosen task string, so two serializations of the same
/// store can differ in LENGTH without differing in which sessions they hold.
///
/// Why: the incident's discarded bytes named only a session already present in
/// the recovered document. Reproducing that needs a shorter document over a
/// longer one whose SESSION SET is identical — a fixture that dropped a session
/// would exercise the orphan-refusal path instead, which is a different test.
/// What: a minimal `SessionRecord` whose `task` is `task`.
/// Test: used by every fixture in this file.
fn record(id: ManagedSessionId, task: &str) -> SessionRecord {
    SessionRecord {
        id,
        tmux_name: format!("tm-5007-{id}"),
        cwd: PathBuf::from("/tmp"),
        task: task.to_string(),
        state: ManagedSessionState::Active,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: false,
        source_id: None,
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
        worktree_owner: None,
        terminal_at: None,
    }
}

/// The exact 2026-08-06 corruption shape, built from real store writes.
///
/// Why: a fixture assembled by hand could be wrong about what the writer
/// actually produces. This one serializes the SAME two sessions twice through
/// the production `SessionStore::save`, once with a long task and once with a
/// short one, then lays the short document over the long one — leaving a
/// complete document followed by the longer document's tail, which is precisely
/// what the incident file contained.
/// What: corrupts `<dir>/sessions.json` in place and returns
/// `(valid_prefix_len, trailing_len, session_ids)`.
/// Test: consumed by `load_reports_a_trailing_tail_as_corruption_with_a_byte_offset`
/// and the repair tests.
async fn corrupt_with_stale_tail(dir: &TempDir) -> (usize, usize, Vec<ManagedSessionId>) {
    let ids = vec![ManagedSessionId::new(), ManagedSessionId::new()];
    let long_task = "x".repeat(400);

    let mut store = SessionStore::load(dir.path()).await.expect("load");
    store
        .upsert_many(ids.iter().map(|id| record(*id, &long_task)).collect())
        .await
        .expect("seed long");
    let path = dir.path().join("sessions.json");
    let long = std::fs::read_to_string(&path).expect("read long");

    // The SAME two sessions, serialized shorter.
    let mut store = SessionStore::load(dir.path()).await.expect("reload");
    store
        .upsert_many(ids.iter().map(|id| record(*id, "short")).collect())
        .await
        .expect("seed short");
    let short = std::fs::read_to_string(&path).expect("read short");

    assert!(
        long.len() > short.len(),
        "fixture invariant: the earlier document must be the longer one"
    );
    let tail = &long[short.len()..];
    std::fs::write(&path, format!("{short}{tail}")).expect("write corrupt");
    (short.len(), tail.len(), ids)
}

/// Why: this is the defect. A store file that is a complete document followed
/// by a stale tail must be reported as CORRUPTION, naming the file and the byte
/// offset where the good data ends — not as the bare
/// `trailing characters at line 3755 column 2` that told the operator nothing.
/// What: builds the incident's exact shape, loads it, and asserts the error is
/// `StoreError::Corrupt` carrying the measured prefix and tail lengths, and
/// that its message names the file, the offset, and the repair command.
/// Test: this test.
#[tokio::test]
async fn load_reports_a_trailing_tail_as_corruption_with_a_byte_offset() {
    let dir = TempDir::new().expect("tempdir");
    let (valid, trailing, _) = corrupt_with_stale_tail(&dir).await;

    let err = SessionStore::load(dir.path())
        .await
        .expect_err("a store with a stale tail must not load");
    let StoreError::Corrupt(integrity) = &err else {
        panic!("expected StoreError::Corrupt, got {err:?}");
    };
    assert_eq!(
        integrity.valid_prefix_bytes,
        Some(valid),
        "the diagnosis must locate the end of the valid document"
    );
    assert_eq!(
        integrity.trailing_bytes(),
        trailing,
        "the diagnosis must count the stale tail"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("sessions.json"),
        "the message must name the file: {msg}"
    );
    assert!(
        msg.contains(&format!("byte {valid}")),
        "the message must name the byte offset: {msg}"
    );
    assert!(
        msg.contains("tm repair session-store"),
        "the message must name the repair command: {msg}"
    );
}

/// Why: the read paths fall back to cached data on ANY store error. That is
/// right for a transient I/O failure and wrong to do silently for corruption,
/// so the two have to be distinguishable at the type level.
/// What: asserts `is_corruption` is true for a corrupt store and false for an
/// I/O error.
/// Test: this test.
#[tokio::test]
async fn corruption_is_distinguished_from_transient_io() {
    let dir = TempDir::new().expect("tempdir");
    corrupt_with_stale_tail(&dir).await;
    let err = SessionStore::load(dir.path()).await.expect_err("corrupt");
    assert!(err.is_corruption(), "a corrupt store is corruption");

    let io = StoreError::Io(std::io::Error::other("nfs hiccup"));
    assert!(
        !io.is_corruption(),
        "a transient I/O error must NOT be classified as corruption"
    );
}

/// Why: `read_file` used to answer EVERY read failure with an empty store. A
/// permissions error or an EIO therefore produced a store with zero sessions,
/// and the next `save()` would have written that emptiness over the operator's
/// entire fleet. Only a genuinely absent file may mean "fresh store".
/// What: puts a DIRECTORY where `sessions.json` belongs — a read error that is
/// not `NotFound` on every platform — and asserts `load` fails instead of
/// reporting an empty store.
/// Test: this test.
#[tokio::test]
async fn load_propagates_a_read_error_instead_of_reporting_an_empty_store() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::create_dir(dir.path().join("sessions.json")).expect("mkdir over the store path");

    let err = SessionStore::load(dir.path())
        .await
        .expect_err("an unreadable store must not load as empty");
    assert!(
        !matches!(err, StoreError::NotFound(_)),
        "an unreadable store is not a missing session: {err:?}"
    );
}

/// Why: an absent store still has to mean "no sessions yet", or a fresh machine
/// could never start. This pins that the stricter error handling above did not
/// break the legitimate case.
/// What: loads an empty directory and asserts an empty store.
/// Test: this test.
#[tokio::test]
async fn load_still_treats_an_absent_store_as_empty() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = SessionStore::load(dir.path()).await.expect("load");
    assert!(store.all().await.expect("all").is_empty());
}

/// Why: the store used to record a reload failure nowhere, so the callers that
/// fell back to cached data left no trace that they had. The degradation must
/// be observable, and must clear once a read succeeds — a sticky one would
/// train operators to ignore it.
/// What: seeds a store, corrupts the file, asserts a failed reload records a
/// corrupt degradation naming the file, then restores the file and asserts the
/// next successful reload clears it.
/// Test: this test.
#[tokio::test]
async fn store_records_and_clears_a_reload_failure() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = SessionStore::load(dir.path()).await.expect("load");
    store
        .upsert(record(ManagedSessionId::new(), "task"))
        .await
        .expect("seed");
    assert!(store.degradation().is_none(), "a fresh store is healthy");

    let path = dir.path().join("sessions.json");
    let good = std::fs::read_to_string(&path).expect("read good");
    std::fs::write(&path, format!("{good}{good}")).expect("append a stale tail");
    assert!(store.reload_if_changed().await.is_err(), "reload must fail");
    let degraded = store.degradation().expect("the failure must be recorded");
    assert!(degraded.corrupt, "a parse failure is corruption");
    assert!(
        degraded.message.contains("sessions.json"),
        "the recorded message names the file: {}",
        degraded.message
    );

    std::fs::write(&path, &good).expect("restore");
    store.reload_if_changed().await.expect("reload succeeds");
    assert!(
        store.degradation().is_none(),
        "a successful reload must clear the degradation"
    );
}

/// Why: `log_reload_fallback` is the severity split that stops corruption being
/// absorbed at the same level as an NFS hiccup. The classification it keys off
/// is the part worth pinning.
/// What: asserts corruption and I/O take the two different branches, and that
/// both are callable.
/// Test: this test.
#[test]
fn log_reload_fallback_separates_corruption_from_a_transient_error() {
    let corrupt = StoreError::Serialize("trailing characters".into());
    let transient = StoreError::Io(std::io::Error::other("nfs hiccup"));
    assert!(corrupt.is_corruption());
    assert!(!transient.is_corruption());
    crate::session_manager::store_health::log_reload_fallback(&corrupt, 3);
    crate::session_manager::store_health::log_reload_fallback(&transient, 3);
}

/// Why (#5007, the issue's hypothesis): the corruption shape — a shorter
/// document over a longer one with the old tail surviving — is what two writers
/// sharing ONE staging file produce. Each opens `sessions.json.tmp` with
/// truncate and streams its own serialization in; the file that gets renamed
/// into place ends up as long as the longer document and as valid as the
/// shorter one. Removing the shared name removes the mechanism.
/// What: two stores over the SAME directory must stage through different files,
/// and neither may be the store itself or live outside its directory (the
/// rename has to stay on one filesystem to remain atomic).
/// Test: this test.
#[tokio::test]
async fn two_stores_over_one_path_do_not_share_a_staging_file() {
    let dir = TempDir::new().expect("tempdir");
    let a = SessionStore::load(dir.path()).await.expect("load A");
    let b = SessionStore::load(dir.path()).await.expect("load B");
    assert_ne!(
        a.staging_path_for_test(),
        b.staging_path_for_test(),
        "two writers of one store must never share a staging file"
    );
    assert_ne!(
        a.staging_path_for_test(),
        dir.path().join("sessions.json"),
        "the staging file must not be the store itself"
    );
    assert_eq!(
        a.staging_path_for_test().parent(),
        Some(dir.path()),
        "the staging file must be a sibling so the rename stays on one filesystem"
    );
}

/// Why: a repair rewrites the operator's only copy of the fleet's state, so it
/// must back up first and cut at the measured document boundary — not at
/// whatever serde's line/column happens to point at.
/// What: repairs the incident-shaped fixture, then asserts the backup holds the
/// original bytes verbatim, the store parses again, both sessions survive, and
/// the store accepts a write — the property the corruption destroyed.
/// Test: this test.
#[tokio::test]
async fn repair_backs_up_then_truncates_to_the_valid_document() {
    let dir = TempDir::new().expect("tempdir");
    let (valid, trailing, ids) = corrupt_with_stale_tail(&dir).await;
    let path = dir.path().join("sessions.json");
    let before = std::fs::read_to_string(&path).expect("read corrupt");

    let now = Utc.with_ymd_and_hms(2026, 8, 6, 9, 1, 17).single().expect("timestamp");
    let outcome = repair(&path, false, now).expect("repair");

    assert_eq!(outcome.kept_bytes, valid);
    assert_eq!(outcome.discarded_bytes, trailing);
    assert_eq!(outcome.sessions, 2);
    assert!(
        outcome.forced_orphans.is_empty(),
        "the incident's tail names no unknown session"
    );
    assert_eq!(
        std::fs::read_to_string(&outcome.backup).expect("read backup"),
        before,
        "the backup must hold the original bytes verbatim"
    );

    let mut store = SessionStore::load(dir.path())
        .await
        .expect("the repaired store must load");
    let all = store.all().await.expect("all");
    assert_eq!(all.len(), 2, "both sessions survive the repair");
    for id in &ids {
        assert!(store.get(id).await.is_ok(), "{id} survived");
    }
    store
        .upsert(record(ManagedSessionId::new(), "post-repair write"))
        .await
        .expect("a repaired store must accept writes");
}

/// Why: truncation discards bytes. If those bytes are the only record of a
/// session, the repair would silently delete it — so the tool must refuse and
/// say which ids are at stake rather than quietly making the file parse.
/// What: appends a tail naming an id absent from the valid document, asserts
/// the refusal names it, and asserts the file was NOT modified.
/// Test: this test.
#[tokio::test]
async fn repair_refuses_an_orphaned_tail_without_force() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = SessionStore::load(dir.path()).await.expect("load");
    store
        .upsert(record(ManagedSessionId::new(), "kept"))
        .await
        .expect("seed");
    let path = dir.path().join("sessions.json");
    let good = std::fs::read_to_string(&path).expect("read good");
    let orphan = ManagedSessionId::new().to_string();
    let corrupt = format!("{good}\n{{\"sessions\":{{\"{orphan}\":null}}}}");
    std::fs::write(&path, &corrupt).expect("write corrupt");

    let err = repair(&path, false, Utc::now()).expect_err("must refuse");
    let RepairError::OrphanedSessions { orphans, .. } = &err else {
        panic!("expected OrphanedSessions, got {err:?}");
    };
    assert!(
        orphans.contains(&orphan),
        "the refusal must name the at-risk id: {orphans:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("re-read"),
        corrupt,
        "a refused repair must not modify the file"
    );
}

/// Why: the refusal has to be overridable — an operator who has looked at the
/// backup and decided the tail is junk needs a way through.
/// What: repairs the same orphaned fixture with `force`, and asserts the
/// discarded id is REPORTED rather than dropped in silence.
/// Test: this test.
#[tokio::test]
async fn repair_with_force_discards_an_orphaned_tail() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = SessionStore::load(dir.path()).await.expect("load");
    store
        .upsert(record(ManagedSessionId::new(), "kept"))
        .await
        .expect("seed");
    let path = dir.path().join("sessions.json");
    let good = std::fs::read_to_string(&path).expect("read good");
    let orphan = ManagedSessionId::new().to_string();
    std::fs::write(
        &path,
        format!("{good}\n{{\"sessions\":{{\"{orphan}\":null}}}}"),
    )
    .expect("write corrupt");

    let outcome = repair(&path, true, Utc::now()).expect("forced repair");
    assert!(
        outcome.forced_orphans.contains(&orphan),
        "a forced repair must report what it discarded: {:?}",
        outcome.forced_orphans
    );
    assert!(
        SessionStore::load(dir.path()).await.is_ok(),
        "the forced repair still produces a loadable store"
    );
}

/// Why: re-running the repair after it has worked must not cut a healthy file.
/// What: asserts a parseable store yields `NotCorrupt`.
/// Test: this test.
#[tokio::test]
async fn plan_refuses_a_healthy_store() {
    let dir = TempDir::new().expect("tempdir");
    let mut store = SessionStore::load(dir.path()).await.expect("load");
    store
        .upsert(record(ManagedSessionId::new(), "task"))
        .await
        .expect("seed");
    let err = plan(&dir.path().join("sessions.json")).expect_err("healthy");
    assert!(matches!(err, RepairError::NotCorrupt(_)), "got {err:?}");
}

/// Why: a file whose HEAD is broken cannot be recovered by truncation, and
/// pretending otherwise would leave the operator with an empty store and a
/// backup they were never told to look at.
/// What: asserts a truncated document is refused with `NoValidPrefix`.
/// Test: this test.
#[test]
fn repair_rejects_a_file_with_no_valid_prefix() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("sessions.json");
    std::fs::write(&path, r#"{"sessions": {"a": "#).expect("write");
    let err = plan(&path).expect_err("no valid prefix");
    assert!(matches!(err, RepairError::NoValidPrefix { .. }), "{err:?}");
}

/// Why: the whole repair rests on this measurement.
/// What: a whole document measures its own length; trailing junk does not move
/// the boundary.
/// Test: this test.
#[test]
fn valid_prefix_bytes_measures_a_whole_document() {
    let doc = r#"{"sessions":{}}"#;
    assert_eq!(valid_prefix_bytes(doc), Some(doc.len()));
    assert_eq!(
        valid_prefix_bytes(&format!("{doc}garbage")),
        Some(doc.len()),
        "trailing junk must not move the document boundary"
    );
}

/// Why: see [`repair_rejects_a_file_with_no_valid_prefix`] — the `None` case is
/// what routes a hopeless file away from truncation.
/// What: an unterminated document measures nothing.
/// Test: this test.
#[test]
fn valid_prefix_bytes_is_none_for_a_truncated_head() {
    assert_eq!(valid_prefix_bytes(r#"{"sessions": {"#), None);
}

/// Why: the byte counts are what an operator checks the tool's arithmetic
/// against before letting it cut their store.
/// What: the tail length is total minus prefix, and the whole file when nothing
/// parsed.
/// Test: this test.
#[test]
fn trailing_bytes_counts_the_discarded_tail() {
    let base = StoreIntegrity {
        path: PathBuf::from("/x/sessions.json"),
        total_bytes: 146_201,
        valid_prefix_bytes: Some(145_090),
        detail: "trailing characters".into(),
        line: 3755,
        column: 2,
    };
    assert_eq!(base.trailing_bytes(), 1111, "the incident's own numbers");
    assert_eq!(
        StoreIntegrity {
            valid_prefix_bytes: None,
            ..base
        }
        .trailing_bytes(),
        146_201,
        "with no valid prefix the whole file is unusable"
    );
}

/// Why: the original message named neither the file nor an offset nor a fix.
/// What: asserts all three appear.
/// Test: this test.
#[test]
fn diagnostic_names_the_file_the_offset_and_the_repair_command() {
    let msg = StoreIntegrity {
        path: PathBuf::from("/x/sessions.json"),
        total_bytes: 146_201,
        valid_prefix_bytes: Some(145_090),
        detail: "trailing characters at line 3755 column 2".into(),
        line: 3755,
        column: 2,
    }
    .diagnostic();
    assert!(msg.contains("/x/sessions.json"), "{msg}");
    assert!(msg.contains("byte 145090"), "{msg}");
    assert!(msg.contains("1111 trailing byte(s)"), "{msg}");
    assert!(msg.contains("tm repair session-store"), "{msg}");
}

/// Why: `analyze` is what turns a serde error into the diagnosis; both of its
/// outcomes need pinning.
/// What: a trailing tail yields a measured prefix.
/// Test: this test.
#[test]
fn analyze_locates_the_valid_prefix_of_a_trailing_tail() {
    let raw = r#"{"sessions":{}}trailing"#;
    let err = serde_json::from_str::<serde_json::Value>(raw).expect_err("must fail");
    let got = analyze(&PathBuf::from("/x/sessions.json"), raw, &err);
    assert_eq!(got.valid_prefix_bytes, Some(15));
    assert_eq!(got.total_bytes, raw.len());
    assert_eq!(got.trailing_bytes(), raw.len() - 15);
}

/// Why: see above.
/// What: a truncated head has no recoverable prefix.
/// Test: this test.
#[test]
fn analyze_reports_no_valid_prefix_for_a_broken_head() {
    let raw = r#"{"sessions": {"#;
    let err = serde_json::from_str::<serde_json::Value>(raw).expect_err("must fail");
    let got = analyze(&PathBuf::from("/x/sessions.json"), raw, &err);
    assert_eq!(got.valid_prefix_bytes, None);
}

/// Why: a second repair must not overwrite the first repair's evidence, and the
/// backup has to be findable next to the file it came from.
/// What: asserts the name shape used by the 2026-08-06 manual backup.
/// Test: this test.
#[test]
fn backup_path_is_a_timestamped_sibling() {
    let now = Utc.with_ymd_and_hms(2026, 8, 6, 9, 1, 17).single().expect("timestamp");
    let got = backup_path(&PathBuf::from("/x/session-manager/sessions.json"), now);
    assert_eq!(
        got,
        PathBuf::from("/x/session-manager/sessions.json.corrupt-backup-20260806-090117")
    );
}

/// Why: the orphan check must read ids from the PARSED document, so a UUID
/// mentioned inside a task description is not mistaken for a surviving session.
/// What: asserts only the map keys count.
/// Test: this test.
#[test]
fn session_ids_reads_the_sessions_map_keys() {
    let head = r#"{"sessions":{"AAAAAAAA-1111-2222-3333-444444444444":{"task":"see bbbbbbbb-1111-2222-3333-444444444444"}}}"#;
    let ids = session_ids(head);
    assert!(ids.contains("aaaaaaaa-1111-2222-3333-444444444444"));
    assert!(
        !ids.contains("bbbbbbbb-1111-2222-3333-444444444444"),
        "a uuid inside a task string is not a session key"
    );
    assert!(session_ids("not json").is_empty());
}

/// Why: the discarded tail is a fragment that cannot be parsed, so the only way
/// to ask "does it name a session?" is to scan it.
/// What: finds canonical UUIDs in a JSON fragment.
/// Test: this test.
#[test]
fn uuid_tokens_finds_ids_in_a_json_fragment() {
    let frag = r#""aaaaAAAA-1111-2222-3333-444444444444": {"state": "active"#;
    let found = uuid_tokens(frag);
    assert!(
        found.contains("aaaaaaaa-1111-2222-3333-444444444444"),
        "{found:?}"
    );
}

/// Why: over-reporting only makes the repair refuse more often; under-reporting
/// would let it delete a session. This pins that plain text yields nothing.
/// What: prose and a non-hex lookalike both yield no ids.
/// Test: this test.
#[test]
fn uuid_tokens_ignores_non_uuid_text() {
    assert!(uuid_tokens("no identifiers here, just prose and 1234-5678").is_empty());
    assert!(
        uuid_tokens("zzzzzzzz-1111-2222-3333-444444444444").is_empty(),
        "non-hex characters are not a uuid"
    );
}
