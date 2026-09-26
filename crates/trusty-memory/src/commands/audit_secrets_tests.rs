//! Tests for `audit secrets --count-only` (#8645).
//!
//! The two contracts that matter: the counts are right, and nothing the scan
//! reads — drawer text, a flagged value, or a token preview — reaches stdout,
//! stderr, tracing or disk. Fake secret values are built at run time so no
//! credential-shaped literal is committed.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use trusty_common::memory_core::palace::{Drawer, Palace, PalaceId};
use trusty_common::memory_core::store::kg_redb::KgStoreRedb;
use trusty_common::memory_core::store::{OpenIntent, PalaceStore};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------- fixtures

/// A mixed-case alphanumeric value of credential length, unique per call.
fn fake_value() -> String {
    let hex = Uuid::new_v4().simple().to_string();
    let mixed: String = hex
        .chars()
        .enumerate()
        .map(|(i, c)| {
            if i % 2 == 0 {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect();
    format!("Zq9{mixed}")
}

/// Write a palace with `contents` as drawers, bypassing the write-time gate.
fn fixture_palace(root: &Path, slug: &str, contents: &[String]) {
    let data_dir = root.join(slug);
    std::fs::create_dir_all(&data_dir).expect("palace dir");
    PalaceStore::save_palace(&Palace {
        id: PalaceId(slug.to_string()),
        name: slug.to_string(),
        description: None,
        created_at: Utc::now(),
        data_dir: data_dir.clone(),
    })
    .expect("save palace");
    let store = KgStoreRedb::open_with_intent(&data_dir.join("kg.redb"), OpenIntent::Writer)
        .expect("open store");
    for c in contents {
        store
            .upsert_drawer(&Drawer::new(Uuid::new_v4(), c))
            .expect("upsert");
    }
}

/// Two palaces with known refusal counts, plus every fake value used.
fn seeded_estate(root: &Path) -> Vec<String> {
    let s: Vec<String> = (0..6).map(|_| fake_value()).collect();
    fixture_palace(
        root,
        "alpha",
        &[
            "The deploy pipeline moved to the new runner this week.".to_string(),
            format!("Rotated the staging credential {} after the review.", s[0]),
            format!("Set DEPLOY_TOKEN={} in the service env file.", s[1]),
            format!(
                "Wrote DB_PASSWORD={} and the spare {} into notes.",
                s[2], s[3]
            ),
            "Fixed in commit 4be103c3f and verified on main.".to_string(),
        ],
    );
    fixture_palace(
        root,
        "beta",
        &[
            "Nothing sensitive lives in this note about retros.".to_string(),
            format!("Pasted {} then API_KEY={} by mistake.", s[4], s[5]),
        ],
    );
    s
}

/// Every file under `root` with its bytes and mtime.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let meta = std::fs::metadata(&path).expect("stat");
                let bytes = std::fs::read(&path).expect("read");
                out.insert(path, (bytes, meta.modified().expect("mtime")));
            }
        }
    }
    out
}

/// True when `haystack` carries any 8-character window of any value.
fn leaks(haystack: &str, values: &[String]) -> bool {
    values.iter().any(|v| {
        v.as_bytes()
            .windows(8)
            .any(|w| haystack.contains(std::str::from_utf8(w).expect("ascii")))
    })
}

/// True when `haystack` carries a `PotentialSecret` preview: the redaction
/// suffix, a value's 4-character preview head, or the refusal message.
fn carries_preview(haystack: &str, values: &[String]) -> bool {
    haystack.contains("…(")
        || haystack.contains("credential token")
        || values.iter().any(|v| haystack.contains(&v[..4]))
}

/// A tracing subscriber that records every span and event field as text.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<String>>);

struct FieldText(Arc<Mutex<String>>);

impl tracing::field::Visit for FieldText {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn Debug) {
        let mut s = self.0.lock().expect("capture lock");
        s.push_str(&format!("{}={value:?}\n", field.name()));
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        span.record(&mut FieldText(self.0.clone()));
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, values: &tracing::span::Record<'_>) {
        values.record(&mut FieldText(self.0.clone()));
    }
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        event.record(&mut FieldText(self.0.clone()));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn row<'a>(rows: &'a [PalaceSecretCounts], palace: &str) -> &'a PalaceSecretCounts {
    rows.iter()
        .find(|r| r.palace == palace)
        .expect("palace row")
}

// ------------------------------------------------------------------- tests

/// Why: the counts are the whole deliverable; a wrong one misleads the owner.
/// What: alpha holds a bare flagged value, a `KEY=value`-only drawer, a mixed
/// drawer whose first flagged token is `KEY=value`, and two clean drawers;
/// beta holds one drawer whose first flagged token is bare.
/// Test: This test.
#[test]
fn counts_refusals_per_palace_and_variant() {
    let root = tempfile::tempdir().expect("root");
    let scratch = tempfile::tempdir().expect("scratch");
    seeded_estate(root.path());

    let rows = scan_palaces(root.path(), None, scratch.path()).expect("scan");
    assert_eq!(rows.len(), 2);
    let alpha = row(&rows, "alpha");
    assert_eq!(
        alpha,
        &PalaceSecretCounts {
            palace: "alpha".into(),
            drawers_scanned: 5,
            drawers_refused: 3,
            by_variant: RejectCounts {
                potential_secret: 3,
                ..Default::default()
            },
            key_value_first: 2,
            key_value_only: 1,
            error: None,
        }
    );
    let beta = row(&rows, "beta");
    assert_eq!(
        (
            beta.drawers_scanned,
            beta.drawers_refused,
            beta.key_value_first,
            beta.key_value_only
        ),
        (2, 1, 0, 0)
    );
    assert_eq!(beta.by_variant.potential_secret, 1);
}

/// Why: #8645's hard rule — the audit never outputs drawer text, a flagged
/// value, or a token preview, on any channel.
/// What: scans and renders (text and JSON) under a capturing tracing
/// subscriber, then asserts no 8-character window of any fake value appears in
/// stdout, stderr or the captured tracing, nor any `PotentialSecret` preview,
/// and that the scratch parent holds nothing afterwards. A probe event, a
/// planted window and `check_secret`'s own message prove the capture and both
/// checks can each fail.
/// Test: This test.
#[test]
fn output_and_tracing_carry_no_drawer_content() {
    let root = tempfile::tempdir().expect("root");
    let scratch = tempfile::tempdir().expect("scratch");
    let values = seeded_estate(root.path());

    let capture = Capture::default();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    // With exactly one live dispatcher, tracing-core registers a callsite
    // against the REGISTERING thread's default — `NoSubscriber` on a parallel
    // test thread — and caches it disabled. A second live dispatcher makes
    // registration consult every dispatcher; the rebuild re-decides callsites
    // that were cached before this test started.
    let _second = tracing::Dispatch::new(capture.clone());
    tracing::subscriber::with_default(capture.clone(), || {
        tracing::callsite::rebuild_interest_cache();
        tracing::info!(probe = "capture-is-live");
        let rows = scan_palaces(root.path(), None, scratch.path()).expect("scan");
        render(&mut out, &mut err, &rows, false).expect("render text");
        render(&mut out, &mut err, &rows, true).expect("render json");
    });
    let traced = capture.0.lock().expect("capture lock").clone();
    let stdout = String::from_utf8(out).expect("utf8");
    let stderr = String::from_utf8(err).expect("utf8");

    assert!(
        traced.contains("capture-is-live"),
        "the capture must be live"
    );
    assert!(
        leaks(&format!("x{}y", &values[0][3..11]), &values),
        "the leak check must be able to fail"
    );
    let real_reject = check_secret(&format!("pasted {} here", values[0]))
        .expect_err("fixture value must be refused")
        .to_string();
    assert!(
        carries_preview(&real_reject, &values),
        "the preview check must recognise check_secret's own message"
    );
    assert!(stdout.contains("palace=alpha scanned=5 refused=3"));
    for (name, text) in [
        ("stdout", &stdout),
        ("stderr", &stderr),
        ("tracing", &traced),
    ] {
        assert!(!leaks(text, &values), "{name} leaked a stored value");
        assert!(
            !carries_preview(text, &values),
            "{name} carried a token preview"
        );
    }
    assert_eq!(
        std::fs::read_dir(scratch.path()).expect("scratch").count(),
        0,
        "the store copy must be deleted when the scan returns"
    );
}

/// Why: the scan must write nothing, both with the daemon stopped (where an
/// open of the live file would run redb's init write transaction) and with it
/// running (where it holds the store open for writing).
/// What: scans with no writer, then again while a `Writer` holds the alpha
/// store, and asserts after each that every file under the data root has the
/// same bytes and mtime, with none added or removed.
/// Test: This test.
#[test]
fn scan_leaves_palace_files_byte_identical_under_a_live_writer() {
    let root = tempfile::tempdir().expect("root");
    let scratch = tempfile::tempdir().expect("scratch");
    seeded_estate(root.path());

    let unlocked = snapshot(root.path());
    scan_palaces(root.path(), None, scratch.path()).expect("unlocked scan");
    assert_eq!(
        snapshot(root.path()),
        unlocked,
        "an unlocked scan must not change, add or touch any palace file"
    );

    let daemon = KgStoreRedb::open_with_intent(
        &root.path().join("alpha").join("kg.redb"),
        OpenIntent::Writer,
    )
    .expect("writer open");
    let locked = snapshot(root.path());
    let rows = scan_palaces(root.path(), None, scratch.path()).expect("locked scan");
    assert_eq!(
        row(&rows, "alpha").drawers_refused,
        3,
        "a held store still scans"
    );
    assert_eq!(
        snapshot(root.path()),
        locked,
        "a scan beside a live writer must not change, add or touch any palace file"
    );
    drop(daemon);
}

/// Why: `--palace` narrows the scan, and a mistyped name must not pass as a
/// clean palace with zero refusals.
/// Test: This test.
#[test]
fn palace_filter_scans_one_and_rejects_an_unknown_name() {
    let root = tempfile::tempdir().expect("root");
    let scratch = tempfile::tempdir().expect("scratch");
    seeded_estate(root.path());

    let rows = scan_palaces(root.path(), Some("beta"), scratch.path()).expect("scan");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].palace, "beta");
    let missing = scan_palaces(root.path(), Some("gamma"), scratch.path());
    assert!(missing.is_err(), "an unknown palace is an error");
}

/// Why: one unreadable palace must not hide the others, and its error line
/// must carry this module's message, never bytes quoted from the store.
/// Test: This test.
#[test]
fn unreadable_palace_is_an_error_row_not_a_fatal_scan() {
    let root = tempfile::tempdir().expect("root");
    let scratch = tempfile::tempdir().expect("scratch");
    let values = seeded_estate(root.path());
    let bad = root.path().join("beta").join("kg.redb");
    std::fs::write(&bad, format!("not a redb file {}", values[4])).expect("corrupt");

    let rows = scan_palaces(root.path(), None, scratch.path()).expect("scan");
    let beta = row(&rows, "beta");
    let error = beta.error.as_deref().expect("beta must report an error");
    assert!(!leaks(error, &values), "an error line leaked store bytes");
    assert_eq!(beta.drawers_scanned, 0);
    assert_eq!(row(&rows, "alpha").drawers_refused, 3);
}

/// Why: the JSON mode is for scripts; it must stay counts only.
/// What: parses the JSON and checks the per-palace keys are exactly the count
/// fields.
/// Test: This test.
#[test]
fn json_output_is_counts_only() {
    let root = tempfile::tempdir().expect("root");
    let scratch = tempfile::tempdir().expect("scratch");
    seeded_estate(root.path());
    let rows = scan_palaces(root.path(), None, scratch.path()).expect("scan");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    render(&mut out, &mut err, &rows, true).expect("render");

    let doc: serde_json::Value = serde_json::from_slice(&out).expect("json");
    let mut keys: Vec<&str> = doc["palaces"][0]
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "by_variant",
            "drawers_refused",
            "drawers_scanned",
            "error",
            "key_value_first",
            "key_value_only",
            "palace"
        ]
    );
    assert_eq!(doc["totals"]["drawers_refused"], 4);
    assert!(err.is_empty());
}

/// Why: the `KEY=value` counters depend on the local tokenizer matching
/// `find_secret_token`'s. If it drifts, a refused drawer can show no flagged
/// token and silently drop out of the counters.
/// Test: This test.
#[test]
fn every_refused_drawer_has_a_flagged_token() {
    let v = fake_value();
    let cases = [
        format!("plain {v} here"),
        format!("in `{v}` code"),
        format!("({v}),"),
        format!("TOKEN={v};"),
        "ordinary prose with nothing to flag".to_string(),
    ];
    for c in &cases {
        assert_eq!(
            check_secret(c).is_err(),
            secret_tokens(c).next().is_some(),
            "tokenizer parity failed for case {}",
            cases.iter().position(|x| x == c).unwrap_or(usize::MAX)
        );
    }
}

/// What: boundary rows for [`is_key_value_shaped`].
/// Test: This test.
#[test]
fn key_value_shape_boundaries() {
    assert!(is_key_value_shaped("API_KEY=abc"));
    assert!(is_key_value_shaped("app.secret-key=abc"));
    assert!(!is_key_value_shaped("abc=="), "base64 padding is not a key");
    assert!(!is_key_value_shaped("=abc"), "empty key");
    assert!(!is_key_value_shaped("KEY="), "empty value");
    assert!(!is_key_value_shaped("a/b=c"), "a path is not a key");
    assert!(!is_key_value_shaped("plainword"));
}

/// Why: `--count-only` names the only mode; a bare `audit secrets` must not
/// run, so a future listing mode can never be reached by omission.
/// Test: This test.
#[test]
fn count_only_flag_is_required() {
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        audit: AuditArgs,
    }
    assert!(Cli::try_parse_from(["audit", "secrets"]).is_err());
    let cli = Cli::try_parse_from([
        "audit",
        "secrets",
        "--count-only",
        "--palace",
        "p",
        "--json",
    ])
    .expect("full form parses");
    let AuditAction::Secrets { palace, json, .. } = cli.audit.action;
    assert_eq!((palace.as_deref(), json), (Some("p"), true));
}
