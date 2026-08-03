//! Unit tests for the resume-from-checkpoint decision surface (issue #3979).
//!
//! Why: [`super::evaluate`] carries the entire safety argument for adopting a
//! partial corpus, so every branch of it is exercised here without a daemon, a
//! filesystem, or a redb file. The end-to-end interrupt/resume behaviour lives
//! in `super::super::resume_tests`.
//!
//! What: table of `evaluate` cases (one per discard reason plus the accept
//! case), the config-fingerprint properties, and the JSON round-trip.
//!
//! Test: this IS the test module.

use super::*;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId};
use std::sync::Arc;
use tokio::sync::RwLock;

/// A checkpoint that would be accepted, so each test can mutate exactly one
/// field and assert that field alone is load-bearing.
fn baseline() -> ReindexCheckpoint {
    ReindexCheckpoint {
        schema_version: CHECKPOINT_SCHEMA_VERSION,
        index_id: "demo".to_string(),
        canonical_root: "/srv/demo".to_string(),
        crate_version: "9.9.9".to_string(),
        config_fingerprint: "fingerprint-a".to_string(),
        created_at_unix: now_unix(),
    }
}

/// Why: the healthy path — an identical checkpoint must adopt, otherwise the
/// feature never fires at all.
/// Test: this test.
#[test]
fn matching_checkpoint_resumes() {
    let c = baseline();
    assert_eq!(evaluate(&c, &c.clone(), 86_400), ResumeDecision::Resume);
}

/// Why: a record written by a future (or older) format must never be read
/// through this struct's field set — `serde` would silently ignore unknown
/// fields and produce a plausible-looking but wrong decision.
/// Test: this test.
#[test]
fn stale_schema_version_is_discarded() {
    let current = baseline();
    let mut found = baseline();
    found.schema_version = CHECKPOINT_SCHEMA_VERSION + 1;
    match evaluate(&found, &current, 86_400) {
        ResumeDecision::Discard { reason } => assert!(reason.contains("schema version")),
        ResumeDecision::Resume => unreachable!("a version mismatch must never resume"),
    }
}

/// Why: adopting another index's staging corpus would splice a foreign corpus
/// into this index. Belt-and-braces against a path-resolution bug.
/// Test: this test.
#[test]
fn foreign_index_id_is_discarded() {
    let current = baseline();
    let mut found = baseline();
    found.index_id = "someone-else".to_string();
    assert!(matches!(
        evaluate(&found, &current, 86_400),
        ResumeDecision::Discard { .. }
    ));
}

/// Why: staged chunk paths are stored RELATIVE to the walk root (#402). A
/// checkpoint from a different root would resolve every staged path against the
/// wrong tree — the #2178 hazard class. Must discard.
/// Test: this test.
#[test]
fn moved_root_is_discarded() {
    let current = baseline();
    let mut found = baseline();
    found.canonical_root = "/srv/somewhere-else".to_string();
    match evaluate(&found, &current, 86_400) {
        ResumeDecision::Discard { reason } => assert!(reason.contains("different tree")),
        ResumeDecision::Resume => unreachable!("a root move must never resume"),
    }
}

/// Why: a different daemon build may chunk, hash, or embed differently, and
/// enumerating which upgrades are benign is not something a running daemon can
/// know. Version change ⇒ rebuild.
/// Test: this test.
#[test]
fn changed_crate_version_is_discarded() {
    let current = baseline();
    let mut found = baseline();
    found.crate_version = "0.0.1".to_string();
    assert!(matches!(
        evaluate(&found, &current, 86_400),
        ResumeDecision::Discard { .. }
    ));
}

/// Why: if the walk filters changed, the staged corpus holds files this run
/// would not have walked (or is missing files it would have).
/// Test: this test.
#[test]
fn changed_config_is_discarded() {
    let current = baseline();
    let mut found = baseline();
    found.config_fingerprint = "fingerprint-b".to_string();
    match evaluate(&found, &current, 86_400) {
        ResumeDecision::Discard { reason } => assert!(reason.contains("config changed")),
        ResumeDecision::Resume => unreachable!("a config change must never resume"),
    }
}

/// Why: the age gate is hygiene, not correctness — but it must actually fire,
/// or an orphaned staging file lives forever.
/// Test: this test.
#[test]
fn stale_by_age_is_discarded() {
    let current = baseline();
    let mut found = baseline();
    found.created_at_unix = now_unix().saturating_sub(10_000);
    match evaluate(&found, &current, 60) {
        ResumeDecision::Discard { reason } => assert!(reason.contains("adoption window")),
        ResumeDecision::Resume => unreachable!("an over-age checkpoint must not resume"),
    }
}

/// Why: operators running multi-day reindexes need a way to switch the age gate
/// off without disabling resume entirely.
/// Test: this test.
#[test]
fn zero_max_age_disables_age_gate() {
    let current = baseline();
    let mut found = baseline();
    found.created_at_unix = now_unix().saturating_sub(10_000_000);
    assert_eq!(evaluate(&found, &current, 0), ResumeDecision::Resume);
}

/// Why: the record crosses a process boundary as JSON, so a field that does not
/// survive the round-trip would compare unequal forever and resume would never
/// fire.
/// Test: this test.
#[test]
fn checkpoint_roundtrips_through_json() {
    let c = baseline();
    let bytes = serde_json::to_vec(&c).expect("serialize");
    let back: ReindexCheckpoint = serde_json::from_slice(&bytes).expect("deserialize");
    assert_eq!(c, back);
}

/// Build a bare handle whose walk-affecting config the caller can mutate.
fn handle_with(extensions: Vec<String>, exclude_globs: Vec<String>) -> IndexHandle {
    let indexer = CodeIndexer::new("fp-test", "/tmp/fp-test");
    let mut h = IndexHandle::bare(
        IndexId::new("fp-test"),
        Arc::new(RwLock::new(indexer)),
        std::path::PathBuf::from("/tmp/fp-test"),
    );
    h.extensions = extensions;
    h.exclude_globs = exclude_globs;
    h
}

/// Why: `indexes.toml` list order is not meaningful, so a pure reordering must
/// not invalidate an otherwise-adoptable checkpoint and force a full rebuild.
/// Test: this test.
#[test]
fn config_fingerprint_is_order_insensitive() {
    let a = handle_with(
        vec!["rs".into(), "py".into()],
        vec!["a/**".into(), "b/**".into()],
    );
    let b = handle_with(
        vec!["py".into(), "rs".into()],
        vec!["b/**".into(), "a/**".into()],
    );
    assert_eq!(config_fingerprint(&a), config_fingerprint(&b));
}

/// Why: a genuine filter change alters which files get walked, so the staged
/// corpus is no longer a prefix of what this run would build.
/// Test: this test.
#[test]
fn config_fingerprint_changes_with_extensions() {
    let a = handle_with(vec!["rs".into()], vec![]);
    let b = handle_with(vec!["rs".into(), "md".into()], vec![]);
    assert_ne!(config_fingerprint(&a), config_fingerprint(&b));
}

/// Why: the kill switch must actually switch off; an operator who suspects
/// resume needs to force pre-#3979 behaviour without downgrading.
/// Test: this test.
#[test]
#[serial_test::serial]
fn resume_disabled_by_env() {
    // SAFETY: `#[serial]` excludes every other env-mutating test in this
    // binary, including the reindex runs in `super::resume_tests` that read
    // this variable.
    unsafe { std::env::set_var("TRUSTY_REINDEX_RESUME", "0") };
    assert!(!resume_enabled());
    unsafe { std::env::set_var("TRUSTY_REINDEX_RESUME", "off") };
    assert!(!resume_enabled());
    unsafe { std::env::set_var("TRUSTY_REINDEX_RESUME", "1") };
    assert!(resume_enabled());
    unsafe { std::env::remove_var("TRUSTY_REINDEX_RESUME") };
    assert!(resume_enabled(), "resume must default to enabled");
}

/// Why: the age knob must be operator-settable, and a garbage value must fall
/// back to the default rather than disabling the gate by accident.
/// Test: this test.
#[test]
#[serial_test::serial]
fn checkpoint_max_age_reads_env() {
    // SAFETY: `#[serial]` — see `resume_disabled_by_env`.
    unsafe { std::env::set_var("TRUSTY_REINDEX_CHECKPOINT_MAX_AGE_SECS", "120") };
    assert_eq!(checkpoint_max_age_secs(), 120);
    unsafe { std::env::set_var("TRUSTY_REINDEX_CHECKPOINT_MAX_AGE_SECS", "not-a-number") };
    assert_eq!(checkpoint_max_age_secs(), DEFAULT_CHECKPOINT_MAX_AGE_SECS);
    unsafe { std::env::remove_var("TRUSTY_REINDEX_CHECKPOINT_MAX_AGE_SECS") };
    assert_eq!(checkpoint_max_age_secs(), DEFAULT_CHECKPOINT_MAX_AGE_SECS);
}
