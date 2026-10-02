//! Tests for the `import kuzu` r5 code-critic findings (#277).
//!
//! One test per finding (MEDIUM-1..3, LOW-1..4; MEDIUM-4 lives in
//! `stop_tests.rs`). Each MEDIUM test was run red against b832e472a before
//! its fix. Fixtures and palace helpers come from `tests.rs`. Synthetic data
//! only; credential-shaped strings are built at run time.

use std::path::PathBuf;

use serde_json::json;
use trusty_common::memory_core::filter::check_secret;
use trusty_common::memory_core::store::Triple;

use super::apply::HandleSink;
use super::palace_io::open_palace_for_write;
use super::report::{format_report, format_totals};
use super::retract::Retraction;
use super::screen::{screen, tally, SecretRule};
use super::tests::{export_of, fixture, memory_row, palace, write_run};
use super::*;

// ── helpers ─────────────────────────────────────────────────────────────

/// A provider-prefixed key the palace's secret screen refuses.
fn provider_key() -> String {
    format!("{}-test-{}", "sk", "FAKEfake0123456789abcdefXYZ")
}

/// A bare mixed-case alphanumeric credential-shaped token.
fn mixed_case_key() -> String {
    format!("{}{}", "Zq7RtY2uWx9", "Kp4Ls8Nm3Bv")
}

/// A store report around `counts`, for the store line.
fn report_of(counts: StoreCounts) -> StoreReport {
    StoreReport {
        store: PathBuf::from("/work/proj/.kuzu-memory"),
        palace: Some("proj-palace".to_string()),
        source: Some("pin file"),
        counts,
        status: StoreStatus::Imported,
        flush_error: None,
    }
}

/// Whether `(subject, predicate, object)` is active in `sink`'s palace.
async fn is_active(sink: &HandleSink, subject: &str, predicate: &str, object: &str) -> bool {
    sink.handle
        .kg
        .query_active(subject)
        .await
        .expect("query")
        .iter()
        .any(|t| t.predicate == predicate && t.object == object)
}

/// `drawer:<uuid>` of the drawer imported for `memory_id`.
fn drawer_of(sink: &HandleSink, memory_id: &str) -> String {
    let tag = format!("source:kuzu-memory/{memory_id}");
    let id = sink
        .handle
        .drawers
        .read()
        .iter()
        .find(|d| d.tags.contains(&tag))
        .map(|d| d.id)
        .expect("imported drawer");
    format!("drawer:{id}")
}

// ── MEDIUM-1: every store-supplied tag and triple string is screened ─────

/// Why (#277 MEDIUM-1): the secret screen covered memory content only, so a
/// credential in a user/session column, a metadata value, an entity name or
/// id, a relationship type or a Memory.id reached the palace as a tag or a
/// triple. Each failing string is dropped; the memory around it still imports.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn store_supplied_tags_and_triples_pass_the_secret_screen() {
    let token = provider_key();
    assert!(
        check_secret(&token).is_err(),
        "the screen refuses the token"
    );
    let tmp = tempfile::tempdir().expect("tmp");
    let sink = HandleSink {
        handle: palace(tmp.path(), "kuzu-r6-m1"),
    };
    let mut v = fixture();
    v["memories"][0]["user_id"] = json!(token);
    v["memories"][1]["session_id"] = json!(token);
    v["memories"][2]["metadata"] = json!(format!("{{\"note\": \"{token}\"}}"));
    v["memories"].as_array_mut().expect("rows").push(memory_row(
        &token,
        "synthetic note four",
        "h4",
    ));
    v["entities"][0]["name"] = json!(token);
    v["entities"][1]["id"] = json!(token);
    v["mentions"][1]["entity_id"] = json!(token);
    v["relates_to"][0]["relationship_type"] = json!(token);

    let c = write_run(&sink, &v, false).await;
    assert_eq!((c.new_memories, c.failed_writes), (3, 0), "{c:?}");
    let drawers = sink.handle.kg.load_drawers().expect("drawers");
    for d in &drawers {
        assert!(
            !d.tags.iter().any(|t| t.contains(&token)),
            "a tag carries the token: {:?}",
            d.tags
        );
    }
    let leaked = |t: &Triple| {
        [&t.subject, &t.predicate, &t.object]
            .iter()
            .any(|s| s.contains(&token))
    };
    let triples = sink.handle.kg.dump_all_triples().expect("dump");
    assert!(!triples.iter().any(leaked), "a triple carries the token");
    // What passed the screen still landed.
    assert!(is_active(&sink, "entity:e-widget", "entity_type", "concept").await);
    let m1 = drawer_of(&sink, "m-1");
    assert!(is_active(&sink, &m1, "mentions", "entity:e-widget").await);
}

// ── MEDIUM-2: --update retracts kuzu edges the source no longer has ─────

/// Why (#277 MEDIUM-2, owner ruling (a)): a MENTIONS or RELATES_TO edge
/// removed in kuzu stayed active in the palace forever. Under `--update` a
/// kuzu-provenance triple on an imported drawer that the source no longer
/// carries is retracted; without `--update` nothing is, and a triple another
/// writer added to the same drawer is never touched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_retracts_kuzu_edges_absent_from_the_source() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sink = HandleSink {
        handle: palace(tmp.path(), "kuzu-r6-m2"),
    };
    write_run(&sink, &fixture(), false).await;
    let (m1, m3) = (drawer_of(&sink, "m-1"), drawer_of(&sink, "m-3"));
    let foreign = Triple {
        subject: m1.clone(),
        predicate: "mentions".to_string(),
        object: "entity:hand-added".to_string(),
        valid_from: chrono::Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: Some("user".to_string()),
    };
    sink.handle.kg.assert(foreign).await.expect("assert");

    let mut edited = fixture();
    edited["mentions"] = json!([edited["mentions"][1], edited["mentions"][2]]);
    edited["relates_to"] = json!([]);
    write_run(&sink, &edited, false).await;
    assert!(
        is_active(&sink, &m1, "mentions", "entity:e-widget").await,
        "no --update: nothing is retracted"
    );

    let c = write_run(&sink, &edited, true).await;
    assert_eq!(c.failed_writes, 0, "{c:?}");
    assert!(!is_active(&sink, &m1, "mentions", "entity:e-widget").await);
    assert!(!is_active(&sink, &m3, "relates_to:shared_entity", &m1).await);
    assert!(
        is_active(&sink, &m3, "mentions", "entity:e-widget").await,
        "an edge the source still has stays"
    );
    assert!(
        is_active(&sink, &m1, "mentions", "entity:hand-added").await,
        "a triple another writer added stays"
    );
}

// ── MEDIUM-3: refusals are tallied by rule class ────────────────────────

/// Why (#277 MEDIUM-3): the store line said how many memories were refused
/// but not which detector rule refused them, so an operator could not tell a
/// real key from a false positive without reading the store. The line now
/// tallies refusals by rule class, and never prints the refused token.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusals_are_tallied_by_rule_class_without_tokens() {
    let (provider, mixed) = (provider_key(), mixed_case_key());
    for t in [&provider, &mixed] {
        assert!(check_secret(t).is_err(), "the screen refuses the fixture");
    }
    let tmp = tempfile::tempdir().expect("tmp");
    let sink = HandleSink {
        handle: palace(tmp.path(), "kuzu-r6-m3"),
    };
    let mut v = fixture();
    v["memories"][1]["content"] = json!(format!("synthetic note carrying {provider}"));
    v["memories"][2]["content"] = json!(format!("synthetic note carrying {mixed}"));
    let c = run_plan(&export_of(&v), "store1", Target::Write(&sink), false).await;
    let line = format_report(&report_of(c), false);
    assert!(
        line.contains("refusals by rule: mixed_case_alnum 1, provider_prefix 1"),
        "{line}"
    );
    for t in [&provider, &mixed] {
        assert!(
            !line.contains(t.as_str()),
            "the line carries a token: {line}"
        );
    }
}

/// Why (#277 MEDIUM-3): the label comes from the refused token's shape, and
/// the refusal decision stays with `check_secret`.
#[test]
fn screen_labels_each_detector_rule_class() {
    let aws = format!("{}{}", "AKIA", "Q3RZ7XK2M9PLW4NB");
    let b64 = format!("{}+{}/{}", "dGhpcyBpcyBh", "IHRlc3Qgb2Yg", "QmFzZTY0ZW5j2");
    for (text, want) in [
        (
            format!("note {}", provider_key()),
            SecretRule::ProviderPrefix,
        ),
        (format!("note {aws}"), SecretRule::AwsKeyId),
        (format!("note {b64}"), SecretRule::Base64Blob),
        (
            format!("note {}", mixed_case_key()),
            SecretRule::MixedCaseAlnum,
        ),
    ] {
        assert!(check_secret(&text).is_err(), "fixture refused: {want:?}");
        assert_eq!(screen(&text), Some(want));
    }
    assert_eq!(screen("an ordinary synthetic note"), None);
    let mut t = super::screen::RuleTally::new();
    tally(&mut t, SecretRule::AwsKeyId);
    tally(&mut t, SecretRule::AwsKeyId);
    assert_eq!(t.get("aws_key_id"), Some(&2));
}

/// Why (#277 MEDIUM-2): each retraction is logged with its memory id on the
/// store line, and a dry run counts what it would retract without writing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_retraction_is_reported_with_its_memory_id() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sink = HandleSink {
        handle: palace(tmp.path(), "kuzu-r6-m2-log"),
    };
    write_run(&sink, &fixture(), false).await;
    let m1 = drawer_of(&sink, "m-1");
    let mut edited = fixture();
    edited["mentions"] = json!([edited["mentions"][1], edited["mentions"][2]]);
    let export = export_of(&edited);

    let dry = run_plan(&export, "store1", Target::DryRun(&sink), true).await;
    assert_eq!(dry.retracted.len(), 1, "{dry:?}");
    assert!(is_active(&sink, &m1, "mentions", "entity:e-widget").await);

    let c = run_plan(&export, "store1", Target::Write(&sink), true).await;
    let want = Retraction {
        memory_id: "m-1".to_string(),
        family: "mentions",
    };
    assert_eq!(c.retracted, vec![want], "{c:?}");
    assert!(matches!(status_for(&c, false), StoreStatus::Imported));
    let line = format_report(&report_of(c), true);
    assert!(
        line.contains("retracted a mentions edge of memory m-1: the source no longer has it"),
        "{line}"
    );
    assert!(!line.contains("e-widget"), "{line}");
}

// ── LOW-1 / LOW-2: every row is accounted for ───────────────────────────

/// Why (#277 LOW-1): rows skipped for empty content were counted but printed
/// nowhere, so the totals did not add up to the store's memory count.
#[test]
fn totals_count_empty_rows_and_every_notice() {
    let counts = StoreCounts {
        memories: 5,
        new_memories: 3,
        skipped_empty: 2,
        ..StoreCounts::default()
    };
    let r = report_of(counts);
    assert!(format_report(&r, false).contains("empty 2)"));
    let totals = format_totals(&[r], false);
    assert!(
        totals.contains("new 3, unchanged 0, changed 0, updated 0, empty 2"),
        "{totals}"
    );
}

/// Why (#277 LOW-2): edges in relationship tables the import does not map
/// (`HAS_KEYWORD`, `CO_OCCURS_WITH`, ...) were dropped uncounted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unmapped_relationship_tables_are_counted_and_reported() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sink = HandleSink {
        handle: palace(tmp.path(), "kuzu-r6-l2"),
    };
    let mut v = fixture();
    v["other_edges"] = json!({"HAS_KEYWORD": 5, "CO_OCCURS_WITH": 0});
    let c = run_plan(&export_of(&v), "store1", Target::Write(&sink), false).await;
    assert_eq!(
        c.unsupported_edges.get("HAS_KEYWORD"),
        Some(&5),
        "{:?}",
        c.unsupported_edges
    );
    assert!(!c.unsupported_edges.contains_key("CO_OCCURS_WITH"));
    let r = report_of(c);
    let want = "not imported: 5 edge(s) in unmapped relationship tables (HAS_KEYWORD 5)";
    assert!(format_report(&r, false).contains(want));
    assert!(format_totals(&[r], false).contains(want));

    v["other_edges"] = json!({});
    v["other_edges_error"] = json!("RuntimeError");
    let c = run_plan(&export_of(&v), "store1", Target::DryRun(&sink), false).await;
    let line = format_report(&report_of(c), false);
    assert!(
        line.contains("could not count unmapped relationship tables (RuntimeError)"),
        "{line}"
    );
}

// ── LOW-3: a locked palace ──────────────────────────────────────────────

/// Why (#277 LOW-3): `PalaceLocked` had no test, and its message blamed the
/// daemon when the holder can be any trusty-memory process.
#[test]
fn locked_palace_is_refused_and_the_message_names_any_holder() {
    let tmp = tempfile::tempdir().expect("tmp");
    drop(palace(tmp.path(), "kuzu-locked"));
    let kg = tmp.path().join("kuzu-locked").join("kg.redb");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    // The dropped handle's writer releases the file asynchronously.
    let _holder = loop {
        match redb::Database::create(&kg) {
            Ok(db) => break db,
            Err(e) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "kg.redb stayed open: {e}"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    };
    let err = match open_palace_for_write(tmp.path(), "kuzu-locked") {
        Ok(_) => panic!("a locked palace must be refused"),
        Err(e) => e,
    };
    assert!(matches!(err, KuzuImportError::PalaceLocked(_)), "{err}");
    let msg = err.to_string();
    for want in ["another process", "trusty-memory stop", "second import"] {
        assert!(msg.contains(want), "missing {want:?} in {msg}");
    }
}

// ── LOW-4: the deprecated alias ─────────────────────────────────────────

/// Why (#277 LOW-4): `migrate kuzu-data --limit N` used to run a limited
/// trial; the forwarder ignored the flag and ran a full import instead.
#[test]
fn deprecated_kuzu_data_refuses_limit_before_any_work() {
    let err = crate::commands::kuzu_migrate::handle_kuzu_data_migrate(
        std::path::Path::new("/nonexistent/store.redb"),
        "p",
        false,
        Some(3),
    )
    .expect_err("--limit is refused");
    assert!(
        format!("{err:#}").contains("--limit is not supported"),
        "{err:#}"
    );
}
