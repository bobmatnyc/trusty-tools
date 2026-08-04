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

/// Two lists that differ only in where their element boundaries fall must
/// fingerprint differently (#4721).
///
/// Why: this is the shipped collision. 0.42.0 rendered a list by
/// `sort().join(",")`, so `["a", "b"]` and `["a,b"]` both became `a,b` and hashed
/// identically — the classic `"ab"+"c" == "a"+"bc"` failure. Those are two
/// genuinely different exclude sets (one excludes two patterns, the other
/// excludes a single pattern containing a comma), so a checkpoint staged under
/// the first would be accepted as valid under the second, and the run would
/// promote a corpus built with the wrong walk filters. That is precisely the
/// outcome the fingerprint exists to prevent, so the fingerprint has to be
/// injective, not merely "usually different".
/// What: builds the two handles and asserts their fingerprints differ.
/// Test: this test.
#[test]
fn config_fingerprint_distinguishes_list_element_boundaries() {
    let two_globs = handle_with(vec![], vec!["a".into(), "b".into()]);
    let one_comma_glob = handle_with(vec![], vec!["a,b".into()]);
    assert_ne!(
        config_fingerprint(&two_globs),
        config_fingerprint(&one_comma_glob),
        "#4721: exclude_globs [\"a\", \"b\"] and [\"a,b\"] are different walk \
         filters and must not share a config fingerprint"
    );
}

/// A field's VALUE must not be able to impersonate the next field's framing
/// (#4721).
///
/// Why: the shipped encoding was `name=value;`, and both names and values come
/// from operator-authored `indexes.toml`. A value containing `;exclude_globs=`
/// therefore forged a field boundary, letting two entirely different
/// configurations render to the identical byte string. It is the same defect as
/// the element-boundary collision one level up, and it is why the fix is
/// length-prefixing rather than "pick a rarer separator" — every candidate
/// delimiter is a legal character in a glob or a path.
/// What: `include_paths = ["x;exclude_globs=y"]` with no exclude globs, versus
/// `include_paths = ["x"]` with `exclude_globs = ["y;exclude_globs="]`. Under the
/// old encoding both render `include_paths=x;exclude_globs=y;exclude_globs=;`.
/// Test: this test.
#[test]
fn config_fingerprint_cannot_be_forged_across_field_boundaries() {
    let mut forged = handle_with(vec![], vec![]);
    forged.include_paths = vec![std::path::PathBuf::from("x;exclude_globs=y")];

    let mut genuine = handle_with(vec![], vec!["y;exclude_globs=".into()]);
    genuine.include_paths = vec![std::path::PathBuf::from("x")];

    assert_ne!(
        config_fingerprint(&forged),
        config_fingerprint(&genuine),
        "#4721: a field value must not be able to forge the framing of a later \
         field — these are two different walk configurations"
    );
}

/// Why: the kill switch must actually switch off; an operator who suspects
/// resume needs to force pre-#3979 behaviour without downgrading. Every accepted
/// spelling has to be covered, including the default when the variable is unset.
///
/// #4721: this replaces a `#[serial]` + `unsafe { set_var }` test. `#[serial]`
/// does not exclude the binary's ~1400 NON-serial tests, so mutating a
/// process-global variable under it is a quieter race, not an absent one — and
/// the same `getenv`/`setenv` data race is why Rust 2024 made `set_var` unsafe
/// in the first place. Splitting the parse out of the env read makes the whole
/// decision testable with no globals at all; the env READ is covered by
/// `env_knob_names_are_wired_up`, which supplies the variables at process spawn.
/// Test: this test.
#[test]
fn resume_enabled_from_parses() {
    for off in ["0", "false", "no", "off", "OFF", " off ", "False"] {
        assert!(
            !resume_enabled_from(Some(off)),
            "{off:?} must disable resume"
        );
    }
    for on in ["1", "true", "yes", "on", "", "banana"] {
        assert!(resume_enabled_from(Some(on)), "{on:?} must leave resume on");
    }
    assert!(
        resume_enabled_from(None),
        "resume must default to enabled when the variable is unset"
    );
}

/// Why: the age knob must be operator-settable, and a garbage value must fall
/// back to the default rather than disabling the gate by accident.
/// Test: this test.
#[test]
fn checkpoint_max_age_from_parses() {
    assert_eq!(checkpoint_max_age_from(Some("120")), 120);
    assert_eq!(checkpoint_max_age_from(Some("  120 ")), 120);
    assert_eq!(checkpoint_max_age_from(Some("0")), 0, "0 disables the gate");
    for garbage in ["not-a-number", "", "-1", "12.5"] {
        assert_eq!(
            checkpoint_max_age_from(Some(garbage)),
            DEFAULT_CHECKPOINT_MAX_AGE_SECS,
            "{garbage:?} must fall back to the default, never disable the gate"
        );
    }
    assert_eq!(
        checkpoint_max_age_from(None),
        DEFAULT_CHECKPOINT_MAX_AGE_SECS
    );
}

/// Why (#4721): the two pure-parse tests above cannot catch a typo in either
/// variable NAME, and the previous `set_var` tests bought that coverage with a
/// process-global race. This buys it with process isolation instead: the child
/// gets both variables at spawn and runs this test alone, so no sibling test can
/// observe or clobber them.
/// What: asserts the real accessors read the documented variable names.
/// Test: this test.
#[test]
fn env_knob_names_are_wired_up() {
    if !super::super::test_isolation::run_isolated(
        "service::reindex::checkpoint::tests::env_knob_names_are_wired_up",
        &[
            ("TRUSTY_REINDEX_RESUME", "0"),
            ("TRUSTY_REINDEX_CHECKPOINT_MAX_AGE_SECS", "120"),
        ],
    ) {
        return;
    }
    assert!(
        !resume_enabled(),
        "resume_enabled() must read TRUSTY_REINDEX_RESUME"
    );
    assert_eq!(
        checkpoint_max_age_secs(),
        120,
        "checkpoint_max_age_secs() must read TRUSTY_REINDEX_CHECKPOINT_MAX_AGE_SECS"
    );
}

/// A checkpoint written by the shipped v1 format must never be adopted (#4721).
///
/// Why: trusty-search 0.42.0 shipped `config_fingerprint` with a separator-based
/// encoding under which two different configurations could hash identically. The
/// encoding is fixed, but records already on disk in the field carry fingerprints
/// computed the old way. They must invalidate CLEANLY — fall back to a full
/// reindex — rather than be compared against a v2 fingerprint and produce a
/// mismatch reason that reads like a config change, or (worse) coincidentally
/// match. The schema-version bump is what guarantees that, and it fires before
/// any fingerprint comparison.
/// What: parses a byte-for-byte v1 record — the exact JSON 0.42.0 writes — and
/// asserts `evaluate` discards it on the version gate, with every other field
/// deliberately matching the current run so nothing ELSE could be doing the
/// rejecting.
/// Test: this test.
#[test]
fn checkpoints_written_by_the_shipped_v1_format_are_discarded() {
    let current = baseline();
    let v1_json = format!(
        r#"{{"schema_version":1,"index_id":"{}","canonical_root":"{}",
            "crate_version":"{}","config_fingerprint":"{}","created_at_unix":{}}}"#,
        current.index_id,
        current.canonical_root,
        current.crate_version,
        current.config_fingerprint,
        current.created_at_unix,
    );
    let found: ReindexCheckpoint =
        serde_json::from_str(&v1_json).expect("a v1 record still parses — the VERSION is the gate");
    match evaluate(&found, &current, 86_400) {
        ResumeDecision::Discard { reason } => assert!(
            reason.contains("schema version"),
            "a 0.42.0 record must be rejected on the version gate, not \
             incidentally on some other field; got: {reason}"
        ),
        ResumeDecision::Resume => {
            unreachable!("#4721: a checkpoint written by 0.42.0 must never be adopted")
        }
    }
}
