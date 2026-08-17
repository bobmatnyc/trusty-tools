//! Behavioural tests for the cross-machine share primitive (#5902).
//!
//! Why: the interesting properties are not expressible against a pure function.
//! "The same fact from two machines becomes one memory" needs two palaces, a
//! file, and a real redb store, because what it is really asserting is that the
//! hash-keyed lookup, the merge rule, and the durable write agree with each
//! other. Each test below names the property it pins.
//! What: two independent on-disk palaces standing in for two machines, driven
//! through `export_palace_jsonl` / `import_palace_jsonl`.
//! Test: this file IS the tests — run with:
//!   cargo test -p trusty-common --features memory-core,embedder-test-support share::

use std::path::Path;
use std::sync::Arc;

use chrono::{Duration, DurationRound, Utc};
use tempfile::tempdir;
use uuid::Uuid;

use super::*;
use crate::memory_core::content_hash::{
    CONTENT_HASH_VERSION, ContentHash, memory_content_hash, normalize_for_hash,
};
use crate::memory_core::palace::{DrawerType, Palace, PalaceId, RoomType};
use crate::memory_core::retrieval::{
    PalaceHandle, RememberOptions, recall_with_default_embedder, seed_shared_embedder_with_mock,
};

// ── Fixtures ────────────────────────────────────────────────────────────────

/// One palace on disk, standing in for one machine.
fn open_palace(root: &Path, id: &str) -> Arc<PalaceHandle> {
    seed_shared_embedder_with_mock();
    let palace = Palace {
        id: PalaceId::new(id),
        name: id.to_string(),
        description: None,
        created_at: Utc::now(),
        data_dir: root.join(id),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    PalaceHandle::open(&palace).unwrap()
}

/// Write a memory the way a user would, then force its `created_at` to `age_days`
/// ago so the timestamp-merge tests can pin a clock they control.
async fn write(handle: &PalaceHandle, content: &str, tags: &[&str]) -> Uuid {
    handle
        .remember_with_options(
            content.to_string(),
            RoomType::General,
            tags.iter().map(|t| t.to_string()).collect(),
            0.5,
            RememberOptions::forced(),
        )
        .await
        .expect("write a memory")
}

/// Backdate a drawer in both redb and the in-memory mirror.
async fn backdate(handle: &PalaceHandle, id: Uuid, days: i64) {
    let mut d = handle
        .drawers
        .read()
        .iter()
        .find(|d| d.id == id)
        .cloned()
        .expect("drawer present");
    d.created_at = Utc::now() - Duration::days(days);
    handle.kg.upsert_drawer(&d).await.unwrap();
    let mut drawers = handle.drawers.write();
    for slot in drawers.iter_mut().filter(|x| x.id == id) {
        *slot = d.clone();
    }
}

fn hashes(handle: &PalaceHandle) -> Vec<ContentHash> {
    let mut h: Vec<ContentHash> = handle
        .drawers
        .read()
        .iter()
        .map(|d| d.content_hash)
        .collect();
    h.sort();
    h
}

/// One drawer's content digest, with the read lock scoped to this call so no
/// caller holds it across an `.await`.
fn hash_of(handle: &PalaceHandle, id: Uuid) -> ContentHash {
    handle
        .drawers
        .read()
        .iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("drawer {id} present"))
        .content_hash
}

fn bodies(handle: &PalaceHandle) -> Vec<String> {
    let mut b: Vec<String> = handle
        .drawers
        .read()
        .iter()
        .map(|d| d.content.clone())
        .collect();
    b.sort();
    b
}

// ── Record shape and verification ───────────────────────────────────────────

#[test]
fn record_round_trips_through_json() {
    let drawer = crate::memory_core::palace::Drawer::new(Uuid::new_v4(), "a stable fact");
    let rec = SharedMemoryRecord::from_drawer(&drawer, "backend");
    let line = serde_json::to_string(&rec).unwrap();
    // The digest travels as readable hex, so a committed file stays greppable.
    assert!(line.contains(&drawer.content_hash.to_hex()), "{line}");
    let back: SharedMemoryRecord = serde_json::from_str(&line).unwrap();
    assert_eq!(back, rec);
    assert_eq!(back.verify().unwrap(), drawer.content_hash);
}

#[test]
fn verify_accepts_a_round_tripped_record() {
    let drawer = crate::memory_core::palace::Drawer::new(Uuid::new_v4(), "verified");
    let rec = SharedMemoryRecord::from_drawer(&drawer, "general");
    assert_eq!(rec.verify().unwrap(), memory_content_hash("verified"));
}

/// Why: a line whose declared digest does not describe its body would enter a
/// palace under an identity no other machine can reproduce — convergence would
/// break silently and permanently. This is the check that makes that
/// unreachable.
/// Test: This test.
#[test]
fn verify_rejects_a_forged_digest() {
    let drawer = crate::memory_core::palace::Drawer::new(Uuid::new_v4(), "the real body");
    let mut rec = SharedMemoryRecord::from_drawer(&drawer, "general");
    rec.content_hash = memory_content_hash("something else entirely");
    match rec.verify() {
        Err(RecordError::DigestMismatch { .. }) => {}
        other => panic!("expected DigestMismatch, got {other:?}"),
    }
}

#[test]
fn verify_rejects_an_unknown_format_version() {
    let drawer = crate::memory_core::palace::Drawer::new(Uuid::new_v4(), "body");
    let mut rec = SharedMemoryRecord::from_drawer(&drawer, "general");
    rec.format_version = SHARE_FORMAT_VERSION + 1;
    match rec.verify() {
        Err(RecordError::UnknownFormatVersion { found }) => {
            assert_eq!(found, SHARE_FORMAT_VERSION + 1)
        }
        other => panic!("expected UnknownFormatVersion, got {other:?}"),
    }
}

/// Why: a record hashed under a different normalization contract carries digests
/// from another identity space. Comparing them with local ones would produce
/// arbitrary hits and misses, so the version mismatch has to be refused rather
/// than tolerated.
/// Test: This test.
#[test]
fn verify_rejects_an_unknown_hash_version() {
    let drawer = crate::memory_core::palace::Drawer::new(Uuid::new_v4(), "body");
    let mut rec = SharedMemoryRecord::from_drawer(&drawer, "general");
    rec.hash_version = CONTENT_HASH_VERSION + 1;
    match rec.verify() {
        Err(RecordError::UnknownHashVersion { found }) => {
            assert_eq!(found, CONTENT_HASH_VERSION + 1)
        }
        other => panic!("expected UnknownHashVersion, got {other:?}"),
    }
}

// ── Normalization, at the level that matters cross-machine ──────────────────

/// Why: this is the whole premise. Two clients that typed the same sentence hold
/// bytes that differ by a line ending, a trailing newline, or a composition form,
/// and they must still write records that collide on hash. Asserting it on
/// `Drawer` rather than on `normalize_for_hash` proves the field, the write path,
/// and the record all carry the property, not just the pure function.
/// Test: This test.
#[tokio::test]
async fn drawers_differing_only_by_whitespace_or_composition_share_one_hash() {
    let tmp = tempdir().unwrap();
    let h = open_palace(tmp.path(), "norm");

    let a = write(&h, "the daemon binds loopback only", &[]).await;
    let b = write(&h, "the daemon binds loopback only\n", &[]).await;
    let c = write(&h, "the daemon binds loopback only\r\n\r\n", &[]).await;
    let d = write(&h, "the daemon binds loopback only   ", &[]).await;

    assert_eq!(
        hash_of(&h, a),
        hash_of(&h, b),
        "a trailing newline is not a new fact"
    );
    assert_eq!(
        hash_of(&h, a),
        hash_of(&h, c),
        "CRLF and blank lines are not a new fact"
    );
    assert_eq!(
        hash_of(&h, a),
        hash_of(&h, d),
        "trailing spaces are not a new fact"
    );

    // The composed / decomposed pair, on its own body.
    let e = write(&h, "the caf\u{0065}\u{0301} rule", &[]).await;
    let f = write(&h, "the caf\u{00e9} rule", &[]).await;
    assert_eq!(
        hash_of(&h, e),
        hash_of(&h, f),
        "NFC must make the two forms one identity"
    );
}

/// Why: normalization is for HASHING only. If it ever leaked into the write path
/// it would be a silent data migration of every palace on disk, rewriting user
/// text nobody asked it to touch.
/// Test: This test.
#[tokio::test]
async fn stored_content_is_never_normalized() {
    let tmp = tempdir().unwrap();
    let h = open_palace(tmp.path(), "verbatim");
    let raw = "  indented\r\nwith trailing space   \n\n";
    let id = write(&h, raw, &[]).await;

    let stored = h
        .drawers
        .read()
        .iter()
        .find(|d| d.id == id)
        .unwrap()
        .content
        .clone();
    assert_eq!(stored, raw, "the caller's bytes must survive verbatim");
    assert_ne!(
        stored,
        normalize_for_hash(raw),
        "the test is vacuous unless normalization would have changed this body"
    );

    // And it survives a round trip through redb, not just the memory mirror.
    let durable =
        h.kg.load_drawers()
            .unwrap()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap();
    assert_eq!(durable.content, raw);
    assert_eq!(durable.content_hash, memory_content_hash(raw));
}

// ── Export ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn export_writes_one_line_per_drawer() {
    let tmp = tempdir().unwrap();
    let h = open_palace(tmp.path(), "exp");
    write(&h, "first fact about the build", &["build"]).await;
    write(&h, "second fact about the daemon", &["daemon"]).await;

    let out = tmp.path().join("share/memories.jsonl");
    let n = export_palace_jsonl(&h, &out).unwrap();
    assert_eq!(n, 2);

    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text.lines().count(), 2);
    for line in text.lines() {
        let rec: SharedMemoryRecord = serde_json::from_str(line).unwrap();
        rec.verify().expect("every exported line verifies");
    }
}

/// Why: the file is committed to git, so its order must be a function of its
/// content and nothing else. Ordering by `created_at` or by drawer UUID would
/// make a re-export produce a diff on a palace that did not change in any way a
/// reader cares about.
/// Test: This test.
#[tokio::test]
async fn export_is_ordered_by_hash() {
    let tmp = tempdir().unwrap();
    let h = open_palace(tmp.path(), "order");
    for body in ["zeta fact", "alpha fact", "mu fact", "beta fact"] {
        write(&h, body, &[]).await;
    }
    let recs = export_palace_records(&h).unwrap();
    let seen: Vec<ContentHash> = recs.iter().map(|r| r.content_hash).collect();
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(seen, sorted, "records must come out in content-hash order");
}

/// Why: an elapsed TTL means the drawer stopped being a fact, and a Tier C slot
/// is a point-in-time claim whose retirement condition is local to the machine
/// that wrote it. Shipping either to another machine asserts something the
/// sender itself no longer believes.
/// Test: This test.
#[tokio::test]
async fn export_skips_expired_and_tier_c() {
    let tmp = tempdir().unwrap();
    let h = open_palace(tmp.path(), "skip");

    write(&h, "an ordinary standing fact", &[]).await;

    // Tier C: a live slot claim.
    h.remember_with_options(
        "pr 4818 is in review".to_string(),
        RoomType::General,
        vec![],
        0.5,
        RememberOptions {
            fact_key: Some("pr:4818/state".to_string()),
            ..RememberOptions::forced()
        },
    )
    .await
    .unwrap();

    // Expired: a TTL already in the past.
    let stale = write(&h, "a fact that has already lapsed", &[]).await;
    {
        let mut drawers = h.drawers.write();
        for d in drawers.iter_mut().filter(|d| d.id == stale) {
            d.expires_at = Some(Utc::now() - Duration::hours(1));
        }
    }

    let recs = export_palace_records(&h).unwrap();
    let bodies: Vec<&str> = recs.iter().map(|r| r.content.as_str()).collect();
    assert_eq!(bodies, vec!["an ordinary standing fact"], "{bodies:?}");
}

// ── Round trip and the four properties ──────────────────────────────────────

#[tokio::test]
async fn export_then_import_preserves_metadata() {
    let tmp = tempdir().unwrap();
    let src = open_palace(tmp.path(), "src");
    let dst = open_palace(tmp.path(), "dst");

    let id = src
        .remember_with_options(
            "the MSRV floor is 1.94".to_string(),
            RoomType::Documentation,
            vec!["msrv".to_string(), "policy".to_string()],
            0.9,
            RememberOptions {
                classify_as: Some(DrawerType::UserFact),
                ..RememberOptions::forced()
            },
        )
        .await
        .unwrap();
    backdate(&src, id, 12).await;
    let original = src
        .drawers
        .read()
        .iter()
        .find(|d| d.id == id)
        .cloned()
        .unwrap();

    let file = tmp.path().join("m.jsonl");
    export_palace_jsonl(&src, &file).unwrap();
    let summary = import_palace_jsonl(&dst, &file).await.unwrap();
    assert_eq!(summary.inserted, 1);

    let imported = dst.drawers.read().first().cloned().unwrap();
    assert_eq!(imported.content, original.content);
    assert_eq!(imported.content_hash, original.content_hash);
    assert_eq!(imported.created_at, original.created_at);
    assert_eq!(imported.tags, original.tags);
    assert_eq!(imported.drawer_type, DrawerType::UserFact);
    assert!((imported.importance - 0.9).abs() < 1e-6);
    // Room ids are UUIDv5 over the canonical key (ADR-0027), so the label
    // round-trips into the same id on a palace that had never seen that room.
    assert_eq!(imported.room_id, original.room_id);
    // The identity is content-derived; the UUID is not, and must not be reused.
    assert_ne!(imported.id, original.id);
}

/// Why: `created_at` is what the earliest-wins merge rule depends on, and an
/// insert that stamped `now` would make every re-import look like fresh
/// knowledge — the failure the rule exists to prevent, moved one step earlier.
/// Test: This test.
#[tokio::test]
async fn import_preserves_the_record_created_at_on_insert() {
    let tmp = tempdir().unwrap();
    let src = open_palace(tmp.path(), "old-src");
    let dst = open_palace(tmp.path(), "old-dst");

    let id = write(&src, "a fact from long ago", &[]).await;
    backdate(&src, id, 200).await;
    let then = src.drawers.read().first().unwrap().created_at;

    let file = tmp.path().join("m.jsonl");
    export_palace_jsonl(&src, &file).unwrap();
    import_palace_jsonl(&dst, &file).await.unwrap();

    let imported = dst.drawers.read().first().cloned().unwrap();
    assert_eq!(imported.created_at, then);
    assert!(
        Utc::now() - imported.created_at > Duration::days(190),
        "the imported drawer must not be stamped with the import time"
    );
}

/// PROPERTY 1 — idempotent by hash. Importing the same export twice changes
/// nothing.
///
/// Why: this is what makes the git workflow safe to run on every pull. If a
/// second import added anything, a repo pulled twice would grow a duplicate of
/// every shared memory.
/// Test: This test.
#[tokio::test]
async fn import_is_idempotent() {
    let tmp = tempdir().unwrap();
    let src = open_palace(tmp.path(), "idem-src");
    let dst = open_palace(tmp.path(), "idem-dst");
    write(&src, "the daemon binds loopback only", &["net"]).await;
    write(&src, "the MSRV floor is 1.94", &["policy"]).await;

    let file = tmp.path().join("m.jsonl");
    export_palace_jsonl(&src, &file).unwrap();

    let first = import_palace_jsonl(&dst, &file).await.unwrap();
    assert_eq!(first.inserted, 2);
    assert!(first.changed_anything());
    let after_first = hashes(&dst);

    let second = import_palace_jsonl(&dst, &file).await.unwrap();
    assert_eq!(
        second,
        ImportSummary {
            inserted: 0,
            merged: 0,
            unchanged: 2,
            skipped: 0
        },
        "a repeated import must be a pure no-op"
    );
    assert!(!second.changed_anything());
    assert_eq!(hashes(&dst), after_first);
    assert_eq!(dst.drawers.read().len(), 2);

    // And a third, to prove it is not merely alternating.
    import_palace_jsonl(&dst, &file).await.unwrap();
    assert_eq!(dst.drawers.read().len(), 2);
}

/// PROPERTY 2 — additive. Importing a superset adds only what is new.
/// Test: This test.
#[tokio::test]
async fn import_of_a_superset_adds_only_the_new() {
    let tmp = tempdir().unwrap();
    let src = open_palace(tmp.path(), "sup-src");
    let dst = open_palace(tmp.path(), "sup-dst");

    write(&src, "shared fact one", &[]).await;
    write(&src, "shared fact two", &[]).await;
    let first_file = tmp.path().join("first.jsonl");
    export_palace_jsonl(&src, &first_file).unwrap();
    import_palace_jsonl(&dst, &first_file).await.unwrap();
    assert_eq!(dst.drawers.read().len(), 2);

    // The sender learns one more thing and re-exports everything.
    write(&src, "shared fact three", &[]).await;
    let second_file = tmp.path().join("second.jsonl");
    export_palace_jsonl(&src, &second_file).unwrap();

    let summary = import_palace_jsonl(&dst, &second_file).await.unwrap();
    assert_eq!(
        summary,
        ImportSummary {
            inserted: 1,
            merged: 0,
            unchanged: 2,
            skipped: 0
        }
    );
    assert_eq!(dst.drawers.read().len(), 3);
    assert_eq!(
        bodies(&dst),
        vec!["shared fact one", "shared fact three", "shared fact two"]
    );
}

/// PROPERTY 3 — convergent. Two machines that both recorded the same fact
/// produce ONE memory, not two.
///
/// Why: this is the defect the whole design exists to fix. `trusty-agents`' key
/// is `imported:{machine_id}:{id}`, so the same sentence written on a laptop and
/// on a desktop stays two records forever. Here the sentence IS the key.
/// Test: This test.
#[tokio::test]
async fn two_machines_converge_on_one_memory() {
    let tmp = tempdir().unwrap();
    let laptop = open_palace(tmp.path(), "laptop");
    let desktop = open_palace(tmp.path(), "desktop");
    let team = open_palace(tmp.path(), "team");

    // The shared fact, typed on both machines with incidental formatting
    // differences no reader would call a difference.
    write(&laptop, "release tags are per-crate", &["release"]).await;
    write(&laptop, "only the laptop knows this", &[]).await;
    write(&desktop, "release tags are per-crate\r\n", &["tagging"]).await;
    write(&desktop, "only the desktop knows this", &[]).await;

    let laptop_file = tmp.path().join("laptop.jsonl");
    let desktop_file = tmp.path().join("desktop.jsonl");
    export_palace_jsonl(&laptop, &laptop_file).unwrap();
    export_palace_jsonl(&desktop, &desktop_file).unwrap();

    import_palace_jsonl(&team, &laptop_file).await.unwrap();
    let second = import_palace_jsonl(&team, &desktop_file).await.unwrap();

    // Three memories, not four: the shared fact merged rather than duplicating.
    assert_eq!(team.drawers.read().len(), 3, "{:?}", bodies(&team));
    assert_eq!(second.inserted, 1, "only the desktop-only fact is new");
    assert_eq!(second.merged, 1, "the shared fact merges");

    // And the merged memory carries both machines' tags.
    let shared = team
        .drawers
        .read()
        .iter()
        .find(|d| d.content_hash == memory_content_hash("release tags are per-crate"))
        .cloned()
        .expect("the shared fact is present exactly once by hash");
    assert!(shared.tags.contains(&"release".to_string()));
    assert!(shared.tags.contains(&"tagging".to_string()));

    // Importing both files a second time, in the other order, still converges.
    import_palace_jsonl(&team, &desktop_file).await.unwrap();
    import_palace_jsonl(&team, &laptop_file).await.unwrap();
    assert_eq!(team.drawers.read().len(), 3);
}

/// PROPERTY 4 — monotone in time. The earlier `created_at` survives a merge, in
/// BOTH import orders.
///
/// Why: `created_at` feeds temporal decay and the recency tie-break in
/// `drawer_listing_order`. If it regressed to whichever import ran last, pulling
/// an old shared file would silently promote every fact in it to "written today"
/// and re-rank the whole palace.
/// Test: This test.
#[tokio::test]
async fn merge_keeps_the_earlier_created_at_in_either_order() {
    let tmp = tempdir().unwrap();

    // Order A: the OLD copy is already local, the NEW one arrives.
    {
        let old = open_palace(tmp.path(), "a-old");
        let new = open_palace(tmp.path(), "a-new");
        let dst = open_palace(tmp.path(), "a-dst");
        let old_id = write(&old, "converging fact", &[]).await;
        backdate(&old, old_id, 100).await;
        let new_id = write(&new, "converging fact", &[]).await;
        backdate(&new, new_id, 1).await;
        let old_at = old.drawers.read().first().unwrap().created_at;

        let f_old = tmp.path().join("a-old.jsonl");
        let f_new = tmp.path().join("a-new.jsonl");
        export_palace_jsonl(&old, &f_old).unwrap();
        export_palace_jsonl(&new, &f_new).unwrap();

        import_palace_jsonl(&dst, &f_old).await.unwrap();
        import_palace_jsonl(&dst, &f_new).await.unwrap();
        assert_eq!(dst.drawers.read().len(), 1);
        assert_eq!(
            dst.drawers.read().first().unwrap().created_at,
            old_at,
            "a later import must not push the timestamp forward"
        );
    }

    // Order B: the NEW copy is already local, the OLD one arrives.
    {
        let old = open_palace(tmp.path(), "b-old");
        let new = open_palace(tmp.path(), "b-new");
        let dst = open_palace(tmp.path(), "b-dst");
        let old_id = write(&old, "converging fact", &[]).await;
        backdate(&old, old_id, 100).await;
        let new_id = write(&new, "converging fact", &[]).await;
        backdate(&new, new_id, 1).await;
        let old_at = old.drawers.read().first().unwrap().created_at;

        let f_old = tmp.path().join("b-old.jsonl");
        let f_new = tmp.path().join("b-new.jsonl");
        export_palace_jsonl(&old, &f_old).unwrap();
        export_palace_jsonl(&new, &f_new).unwrap();

        import_palace_jsonl(&dst, &f_new).await.unwrap();
        import_palace_jsonl(&dst, &f_old).await.unwrap();
        assert_eq!(dst.drawers.read().len(), 1);
        assert_eq!(
            dst.drawers.read().first().unwrap().created_at,
            old_at,
            "an earlier arrival must pull the timestamp back"
        );

        // The earlier timestamp is durable, not just in the memory mirror.
        // `DrawerRecord.created_at_ms` is whole milliseconds, so the redb round
        // trip truncates sub-millisecond precision — a pre-existing property of
        // the store, not of this merge. Compare at the resolution the store
        // keeps rather than pretending it keeps more.
        let durable = dst.kg.load_drawers().unwrap();
        assert_eq!(durable.len(), 1);
        assert_eq!(
            durable[0].created_at,
            old_at.duration_trunc(Duration::milliseconds(1)).unwrap()
        );
    }
}

#[tokio::test]
async fn merge_unions_tags_and_takes_the_higher_importance() {
    let tmp = tempdir().unwrap();
    let a = open_palace(tmp.path(), "tag-a");
    let b = open_palace(tmp.path(), "tag-b");
    let dst = open_palace(tmp.path(), "tag-dst");

    a.remember_with_options(
        "one fact".to_string(),
        RoomType::General,
        vec!["alpha".to_string()],
        0.3,
        RememberOptions::forced(),
    )
    .await
    .unwrap();
    b.remember_with_options(
        "one fact".to_string(),
        RoomType::General,
        vec!["beta".to_string()],
        0.8,
        RememberOptions::forced(),
    )
    .await
    .unwrap();

    let fa = tmp.path().join("ta.jsonl");
    let fb = tmp.path().join("tb.jsonl");
    export_palace_jsonl(&a, &fa).unwrap();
    export_palace_jsonl(&b, &fb).unwrap();
    import_palace_jsonl(&dst, &fa).await.unwrap();
    import_palace_jsonl(&dst, &fb).await.unwrap();

    let merged = dst.drawers.read().first().cloned().unwrap();
    assert_eq!(merged.tags, vec!["alpha".to_string(), "beta".to_string()]);
    assert!(
        (merged.importance - 0.8).abs() < 1e-6,
        "a merge may add information, never remove it"
    );
}

/// Why: an imported memory that cannot be recalled is not a memory. This is the
/// end of the chain the re-embed decision rests on — the vector is rebuilt on
/// this side, so the fact has to come back out of `recall`.
/// Test: This test.
#[tokio::test]
async fn imported_memory_is_recallable() {
    let tmp = tempdir().unwrap();
    let src = open_palace(tmp.path(), "rec-src");
    let dst = open_palace(tmp.path(), "rec-dst");
    write(&src, "the embedder runs bundled ORT by default", &[]).await;

    let file = tmp.path().join("m.jsonl");
    export_palace_jsonl(&src, &file).unwrap();
    import_palace_jsonl(&dst, &file).await.unwrap();

    let hits = recall_with_default_embedder(&dst, "embedder ORT", 5)
        .await
        .expect("recall");
    assert!(
        hits.iter()
            .any(|r| r.drawer.content.contains("bundled ORT")),
        "the imported memory must be reachable through recall"
    );
}

/// Why: a file pulled from git can carry one bad line — a conflict marker, a
/// truncated tail. Refusing the whole file for it would strand every good memory
/// in it.
/// Test: This test.
#[tokio::test]
async fn import_skips_a_bad_line_and_keeps_the_rest() {
    let tmp = tempdir().unwrap();
    let src = open_palace(tmp.path(), "bad-src");
    let dst = open_palace(tmp.path(), "bad-dst");
    write(&src, "a good fact", &[]).await;
    let file = tmp.path().join("m.jsonl");
    export_palace_jsonl(&src, &file).unwrap();

    // Splice in one unparseable line and one whose digest is a forgery.
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str("<<<<<<< HEAD\n");
    let forged = {
        let d = crate::memory_core::palace::Drawer::new(Uuid::new_v4(), "a forged fact");
        let mut r = SharedMemoryRecord::from_drawer(&d, "general");
        r.content_hash = memory_content_hash("a different body");
        serde_json::to_string(&r).unwrap()
    };
    text.push_str(&forged);
    text.push('\n');
    std::fs::write(&file, text).unwrap();

    let summary = import_palace_jsonl(&dst, &file).await.unwrap();
    assert_eq!(summary.inserted, 1);
    assert_eq!(summary.skipped, 2, "{summary:?}");
    assert_eq!(bodies(&dst), vec!["a good fact"]);
}

#[tokio::test]
async fn import_of_an_unknown_drawer_type_falls_back_to_unknown() {
    let tmp = tempdir().unwrap();
    let dst = open_palace(tmp.path(), "type-dst");
    let d = crate::memory_core::palace::Drawer::new(Uuid::new_v4(), "from the future");
    let mut rec = SharedMemoryRecord::from_drawer(&d, "general");
    rec.drawer_type = "SomethingNewerThanThisBuild".to_string();

    let summary = import_palace_records(&dst, &[rec]).await.unwrap();
    assert_eq!(summary.inserted, 1);
    assert_eq!(
        dst.drawers.read().first().unwrap().drawer_type,
        DrawerType::Unknown
    );
}

/// Why: PR 2 merges a committed file with a fresh local export before writing
/// one file back. That merge has to converge by the same rule the palace does,
/// or the committed artefact and the palace would disagree.
/// Test: This test.
#[test]
fn merge_records_converges_two_machines_exports() {
    let old = {
        let mut d = crate::memory_core::palace::Drawer::new(Uuid::new_v4(), "shared");
        d.created_at = Utc::now() - Duration::days(50);
        d.tags = vec!["a".to_string()];
        d.importance = 0.2;
        SharedMemoryRecord::from_drawer(&d, "general")
    };
    let new = {
        let mut d = crate::memory_core::palace::Drawer::new(Uuid::new_v4(), "shared");
        d.created_at = Utc::now();
        d.tags = vec!["b".to_string()];
        d.importance = 0.7;
        SharedMemoryRecord::from_drawer(&d, "general")
    };
    let only_new = {
        let d = crate::memory_core::palace::Drawer::new(Uuid::new_v4(), "unique to the second set");
        SharedMemoryRecord::from_drawer(&d, "general")
    };

    let merged = merge_records(&[&[new.clone(), only_new], std::slice::from_ref(&old)]);
    assert_eq!(merged.len(), 2);
    let shared = merged
        .iter()
        .find(|r| r.content_hash == memory_content_hash("shared"))
        .unwrap();
    assert_eq!(shared.created_at, old.created_at, "earliest wins");
    assert_eq!(shared.tags, vec!["b".to_string(), "a".to_string()]);
    assert!((shared.importance - 0.7).abs() < 1e-6);
}

// ── Supersession ────────────────────────────────────────────────────────────

/// Why: an edited memory is a new hash, so the old one must not orphan. This
/// asserts both halves: the replacement gets a genuinely different identity, and
/// the original gains the `superseded_by` edge that leads to it.
/// Test: This test.
#[tokio::test]
async fn supersede_mints_a_new_hash_and_links_the_original() {
    let tmp = tempdir().unwrap();
    let h = open_palace(tmp.path(), "sup");
    let original = write(&h, "the MSRV floor is 1.90", &["policy"]).await;
    let original_hash = h.drawers.read().first().unwrap().content_hash;

    let outcome = supersede_drawer(
        &h,
        original,
        "the MSRV floor is 1.94",
        RoomType::General,
        vec!["policy".to_string()],
        0.9,
    )
    .await
    .unwrap();
    assert!(outcome.linked, "the edge must land on a healthy palace");

    let replacement = h
        .drawers
        .read()
        .iter()
        .find(|d| d.id == outcome.replacement)
        .cloned()
        .unwrap();
    assert_ne!(
        replacement.content_hash, original_hash,
        "an edited body is a different identity"
    );
    assert_eq!(
        replacement.content_hash,
        memory_content_hash("the MSRV floor is 1.94")
    );

    // The original is still there — supersession adds an edge, it does not
    // delete (ADR-0028 D6).
    assert!(h.drawers.read().iter().any(|d| d.id == original));

    let edges: Vec<_> =
        h.kg.query_active(&format!("drawer:{original}"))
            .await
            .unwrap()
            .into_iter()
            .filter(|t| t.predicate == SUPERSEDED_BY)
            .collect();
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert_eq!(edges[0].object, format!("drawer:{}", outcome.replacement));
}

/// Why (issue #1713): the property that makes supersession safe is that an
/// original is only ever retired once its provenance edge is DURABLE. Before
/// #1713 the dream cycle pushed originals onto the eviction list whether or not
/// the triple write landed, so a canonical drawer could exist with the original
/// gone and no link back to it. Both callers now depend on this one writer
/// returning `Err` rather than swallowing the failure, so that is what this pins.
///
/// The injection is the one `dream::tests::apply_consolidation_result_keeps_original_when_kg_write_fails`
/// already uses: hold the redb file's exclusive flock, so the next
/// `KnowledgeGraph::open` falls back to a read-only snapshot (issue #59) and
/// every write against it is refused.
/// Test: This test.
#[tokio::test]
async fn assert_superseded_by_fails_loud_on_an_unwritable_kg() {
    use crate::memory_core::store::kg::KnowledgeGraph;
    use crate::memory_core::store::kg_redb::KgStoreRedb;

    let dir = tempdir().unwrap();
    let kg_path = dir.path().join("kg.redb");
    drop(KgStoreRedb::open(&kg_path).unwrap());
    let _live = redb::Database::create(&kg_path).unwrap();

    let kg = KnowledgeGraph::open(&kg_path).unwrap();
    assert!(
        kg.is_read_only(),
        "precondition: the KG must be a read-only snapshot for this test to mean anything"
    );

    let err = assert_superseded_by(&kg, Uuid::new_v4(), Uuid::new_v4(), "share:supersede")
        .await
        .expect_err("a failed provenance write must surface, never be swallowed");
    assert!(
        err.to_string().contains("superseded_by"),
        "the error must name what failed: {err:#}"
    );
}

/// Why: the other half of the #1713 property on the share path — when the
/// replacement itself cannot be written, `supersede_drawer` must fail without
/// having touched the original. A partial supersession that removed or demoted
/// the original while its replacement did not exist is the unrecoverable case.
/// Test: This test.
#[tokio::test]
async fn supersede_leaves_the_original_intact_when_the_replacement_cannot_be_written() {
    let tmp = tempdir().unwrap();
    let h = open_palace(tmp.path(), "sup-fail");
    let original = write(&h, "a fact due for correction", &[]).await;
    let before = h.drawers.read().len();

    // A body the write path rejects outright: a raw credential. The secret gate
    // runs even under `force` (#2520), so the replacement never lands.
    let outcome = supersede_drawer(
        &h,
        original,
        "the token is ghp_0123456789abcdefghijklmnopqrstuvwxyzA",
        RoomType::General,
        vec![],
        0.5,
    )
    .await;
    assert!(
        outcome.is_err(),
        "a refused replacement must fail the supersession, not report success"
    );

    assert_eq!(h.drawers.read().len(), before, "nothing was added");
    assert!(
        h.drawers.read().iter().any(|d| d.id == original),
        "the original must survive a failed supersession"
    );
    let durable = h.kg.load_drawers().unwrap();
    assert!(durable.iter().any(|d| d.id == original));
    // And no dangling edge was left pointing at a replacement that never existed.
    let edges: Vec<_> =
        h.kg.query_active(&format!("drawer:{original}"))
            .await
            .unwrap()
            .into_iter()
            .filter(|t| t.predicate == SUPERSEDED_BY)
            .collect();
    assert!(edges.is_empty(), "{edges:?}");
}
