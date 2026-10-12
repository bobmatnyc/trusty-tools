//! Tests for applying a review ledger at import (#8624, PR-C).
//!
//! Every token is synthetic. The fail-open tests (rules 3, 5, 6, 7, 9) were
//! each run red against a deliberately broken variant before this landed.

use std::path::Path;

use crate::engine::filter::{classify_secret, find_secret_spans, SECRET_FILTER_VERSION};
use serde_json::json;

use super::super::apply::{plan_store, HandleSink, PalaceView};
use super::super::ledger::Ledger;
use super::super::mapping::OVERRIDE_TAG;
use super::super::screen::screen;
use super::super::tests::{
    args_for, env, export_of, fixture, memory_row, palace, store_on_disk, FakeDaemon, FakeRunner,
};
use super::super::{run_import_in, run_plan_with, KuzuImportError, Target};
use super::admit::{apply_verdicts, decide, reason, Decision};
use super::export::{classify_mapped, ledger_key, MaskedRecord, RowVerdict, SECRET_SHAPED_ID};
use super::export_tests::{leaked_windows, mixed_token, provider_token};
use super::ledger::{parse_ledger, Lookup, ReviewLedger, LEDGER_FILE, SEAL_FILE};
use super::ledger_tests::{
    exported, high_b64_token, high_token, ledger_bytes, low_ent_b64_token, low_token,
    short_b64_token, threshold_b64_token, threshold_token, STORE,
};
use super::ops::{apply_ops, OpError};
use super::verdict::{sha256_hex, LedgerEntry, RedactOp, Verdict};

fn ledger_of(entries: &[LedgerEntry]) -> ReviewLedger {
    parse_ledger(Path::new("/synthetic/ledger.jsonl"), &ledger_bytes(entries)).expect("parses")
}

fn entry(key: &str, verdict: Verdict) -> LedgerEntry {
    LedgerEntry::new(key.to_string(), SECRET_FILTER_VERSION, verdict)
}

fn redact(spans: &[usize]) -> Verdict {
    Verdict::AdmitRedacted {
        ops: spans
            .iter()
            .map(|&span| RedactOp::RedactSpan { span })
            .collect(),
    }
}

/// Why (#8624 rule 5): each op edits only its own span or line, by index.
/// Test: itself.
#[test]
fn ops_edit_the_named_spans_only() {
    let text = format!("a {} b\nline {} c\nend", mixed_token(), high_token());
    let spans = find_secret_spans(&text);
    assert_eq!(spans.len(), 2);
    let ops = [
        RedactOp::RedactSpan { span: 0 },
        RedactOp::DropLine { span: 1 },
    ];
    assert_eq!(
        apply_ops(&text, &spans, &ops).expect("applies"),
        "a [REDACTED:MIXED_CASE_ALNUM] b\nend"
    );
    let ops = [
        RedactOp::DeleteSpan { span: 1 },
        RedactOp::DropLine { span: 0 },
    ];
    assert_eq!(
        apply_ops(&text, &spans, &ops).expect("applies"),
        "line  c\nend"
    );
    let one_line = format!("{} and {}", mixed_token(), high_token());
    let spans = find_secret_spans(&one_line);
    let ops = [
        RedactOp::DropLine { span: 0 },
        RedactOp::RedactSpan { span: 1 },
    ];
    assert_eq!(apply_ops(&one_line, &spans, &ops).expect("contained"), "");
    // Every rule's redaction marker passes the screen it is re-checked by.
    let b64 = format!("{}+{}/{}", "dGhpcyBpcyBh", "IHRlc3Qgb2Yg", "QmFzZTY0ZW5j2");
    for tok in [b64, mixed_token()] {
        let text = format!("k {tok} v");
        let spans = find_secret_spans(&text);
        let out = apply_ops(&text, &spans, &[RedactOp::RedactSpan { span: 0 }]).expect("applies");
        assert!(
            out.starts_with("k [REDACTED:") && screen(&out).is_none(),
            "{out}"
        );
    }
}

/// Why (#8624 rule 6 — fail-open): an op naming a span the row does not
/// have refuses the row; it is never skipped while the others apply.
/// Test: itself.
#[test]
fn out_of_range_op_refuses_the_row() {
    let content = format!("deploy {} key", high_token());
    let (m, record) = exported("m-1", &content);
    let text = m.content.trim();
    let spans = find_secret_spans(text);
    let ops = [
        RedactOp::RedactSpan { span: 0 },
        RedactOp::DeleteSpan { span: 7 },
    ];
    assert_eq!(
        apply_ops(text, &spans, &ops),
        Err(OpError::OutOfRange {
            op: 1,
            span: 7,
            spans: 1
        })
    );
    let e = entry(&record.key, Verdict::AdmitRedacted { ops: ops.to_vec() });
    assert_eq!(
        decide(&record, text, Lookup::Entry(&e)),
        Decision::Refuse(reason::BAD_OP)
    );
    let e = entry(&record.key, redact(&[0]));
    assert!(matches!(
        decide(&record, text, Lookup::Entry(&e)),
        Decision::Redacted(_)
    ));
}

/// A row with one carve-out span and a valid `admit` verdict for it.
fn admitted_row() -> (String, MaskedRecord, LedgerEntry) {
    let (m, record) = exported("m-1", &format!("label {}", low_token()));
    let good = entry(&record.key, Verdict::Admit {});
    assert_eq!(
        decide(&record, m.content.trim(), Lookup::Entry(&good)),
        Decision::Override
    );
    (m.content.trim().to_string(), record, good)
}

/// Why (#8624 rule 3 — fail-open): a verdict recorded under another secret
/// filter version gives no verdict, even with a matching digest.
/// Test: itself.
#[test]
fn verdict_under_another_filter_version_refuses_the_row() {
    let (text, record, _) = admitted_row();
    let other = LedgerEntry::new(
        record.key.clone(),
        SECRET_FILTER_VERSION + 1,
        Verdict::Admit {},
    );
    assert!(other.digest_matches());
    assert_eq!(
        decide(&record, &text, Lookup::Entry(&other)),
        Decision::Refuse(reason::FILTER_VERSION)
    );
}

/// Why (#8624 rule 3 — fail-open): a verdict recorded for a row's old
/// content does not apply once the content changed.
/// Test: itself.
#[test]
fn verdict_for_changed_content_refuses_the_row() {
    let (text, _, good) = admitted_row();
    let ledger = ledger_of(std::slice::from_ref(&good));
    let (m2, changed) = exported("m-1", &format!("{text} and more"));
    let found = ledger.lookup(&changed.key);
    assert_eq!(found, Lookup::Missing);
    assert_eq!(
        decide(&changed, m2.content.trim(), found),
        Decision::Refuse(reason::NO_VERDICT)
    );
}

/// Why (#8624 rule 3 — fail-open): a line whose digest no longer matches its
/// fields (an edited digest, or a verdict flipped after recording) refuses,
/// and so does a key with two lines.
/// Test: itself.
#[test]
fn verdict_with_a_changed_digest_refuses_the_row() {
    let (text, record, good) = admitted_row();
    let mut edited = good.clone();
    edited.digest = sha256_hex(b"edited");
    assert_eq!(
        decide(&record, &text, Lookup::Entry(&edited)),
        Decision::Refuse(reason::DIGEST)
    );
    let mut flipped = entry(&record.key, Verdict::KeepRefused {});
    flipped.verdict = Verdict::Admit {};
    assert_eq!(
        decide(&record, &text, Lookup::Entry(&flipped)),
        Decision::Refuse(reason::DIGEST)
    );
    assert_eq!(
        decide(&record, &text, Lookup::Duplicate),
        Decision::Refuse(reason::DUPLICATE)
    );
}

/// Why (#8624 rule 9 — fail-open): redacted content is screened again, and a
/// row still flagged after its ops stays refused.
/// Test: itself.
#[test]
fn redaction_that_leaves_a_flagged_span_refuses_the_row() {
    let content = format!("a {} and {} b", mixed_token(), high_token());
    let (m, record) = exported("m-1", &content);
    let text = m.content.trim();
    assert_eq!(record.spans.len(), 2);
    let one = entry(&record.key, redact(&[0]));
    assert_eq!(
        decide(&record, text, Lookup::Entry(&one)),
        Decision::Refuse(reason::STILL_FLAGGED)
    );
    let both = entry(&record.key, redact(&[0, 1]));
    let Decision::Redacted(out) = decide(&record, text, Lookup::Entry(&both)) else {
        panic!("both spans redacted");
    };
    assert_eq!(
        out,
        "a [REDACTED:MIXED_CASE_ALNUM] and [REDACTED:MIXED_CASE_ALNUM] b"
    );
    let (m, record) = exported("m-2", &mixed_token());
    let gone = entry(
        &record.key,
        Verdict::AdmitRedacted {
            ops: vec![RedactOp::DeleteSpan { span: 0 }],
        },
    );
    assert_eq!(
        decide(&record, m.content.trim(), Lookup::Entry(&gone)),
        Decision::Refuse(reason::EMPTY)
    );
}

/// Why (#8624 rule 8): a `base64_blob` is admitted verbatim only below 4.5
/// bits/char (rounded) or under 24 chars; at the threshold it refuses.
/// Test: itself.
#[test]
fn carve_out_admits_low_entropy_and_refuses_at_the_threshold() {
    let cases = [
        (low_token(), true),
        (short_b64_token(), true),
        (low_ent_b64_token(), true),
        (threshold_b64_token(), false),
        (high_b64_token(), false),
    ];
    for (tok, admitted) in cases {
        let shape = classify_secret(&tok);
        let (m, record) = exported("m-1", &format!("x {tok}"));
        let e = entry(&record.key, Verdict::Admit {});
        let want = if admitted {
            Decision::Override
        } else {
            Decision::Refuse(reason::NOT_CARVE_OUT)
        };
        assert_eq!(
            decide(&record, m.content.trim(), Lookup::Entry(&e)),
            want,
            "len {} ent {}",
            shape.len,
            shape.ent_tenths
        );
    }
    assert_eq!(
        [
            threshold_b64_token(),
            short_b64_token(),
            low_ent_b64_token()
        ]
        .map(|t| {
            let s = classify_secret(&t);
            (s.len, s.ent_tenths)
        }),
        [(24, 45), (23, 45), (24, 44)]
    );
}

/// Why (#8624 owner ruling): a `mixed_case_alnum` span is admissible whatever
/// its entropy or length; only `base64_blob` carries the entropy limit.
/// Test: itself.
#[test]
fn carve_out_admits_a_high_entropy_mixed_case_alnum_span() {
    for tok in [high_token(), threshold_token()] {
        let shape = classify_secret(&tok);
        assert!(shape.len >= 24 && shape.ent_tenths >= 45, "{shape:?}");
        let (m, record) = exported("m-1", &format!("x {tok}"));
        assert_eq!(record.spans[0].rule, "mixed_case_alnum");
        let e = entry(&record.key, Verdict::Admit {});
        assert_eq!(
            decide(&record, m.content.trim(), Lookup::Entry(&e)),
            Decision::Override
        );
    }
}

/// A store whose rows exercise every review outcome.
fn review_fixture() -> serde_json::Value {
    let mut v = fixture();
    v["memories"] = json!([
        memory_row(
            "m-1",
            "synthetic note about the widget factory layout",
            "h1"
        ),
        memory_row("m-2", &format!("deploy key {} rotated", high_token()), "h2"),
        memory_row("m-3", &format!("label {}", low_token()), "h3"),
        memory_row("m-4", &format!("x {}", threshold_b64_token()), "h4"),
        memory_row(
            "m-5",
            &format!("a {} and {} b", mixed_token(), high_token()),
            "h5"
        ),
        memory_row(
            "m-6",
            &format!("ci token {} in env", provider_token()),
            "h6"
        ),
    ]);
    v
}

/// The verdicts for [`review_fixture`]; m-4 and m-6 are forged admits.
fn review_entries() -> Vec<LedgerEntry> {
    let content = |i: usize| {
        review_fixture()["memories"][i]["content"]
            .as_str()
            .map(str::to_string)
            .expect("content")
    };
    let key = |i: usize, id: &str| ledger_key(STORE, id, &content(i));
    vec![
        entry(&key(1, "m-2"), redact(&[0])),
        entry(&key(2, "m-3"), Verdict::Admit {}),
        entry(&key(3, "m-4"), Verdict::Admit {}),
        entry(&key(4, "m-5"), redact(&[0])),
        entry(&key(5, "m-6"), redact(&[0])),
    ]
}

/// Why (#8624 rule 7 — fail-open): an auto-refuse row is re-classified from
/// the live row, so a forged ledger line with a valid digest, of either
/// admit kind, never admits it; a secret-shaped id is auto-refused too.
/// Test: itself.
#[test]
fn auto_refuse_row_is_never_admitted_even_with_a_forged_verdict() {
    let content = format!("ci token {} in env", provider_token());
    let key = ledger_key(STORE, "m-6", &content);
    for forged in [
        Verdict::Admit {},
        redact(&[0]),
        Verdict::AdmitRedacted {
            ops: vec![RedactOp::DropLine { span: 0 }],
        },
    ] {
        let mut v = fixture();
        v["memories"] = json!([memory_row("m-6", &content, "h6")]);
        let mut plan = plan_store(&export_of(&v), STORE, &Ledger::default(), &|_| false);
        apply_verdicts(&mut plan, &ledger_of(&[entry(&key, forged.clone())]));
        let (m, _) = &plan.memories[0];
        assert_eq!(
            (plan.counts.review_admitted, plan.counts.review_overrides),
            (0, 0),
            "{forged:?}"
        );
        assert_eq!(
            plan.counts.review_refusals.get(reason::AUTO_REFUSE),
            Some(&1)
        );
        assert_eq!(
            (m.content.as_str(), m.review_override),
            (content.as_str(), false)
        );
    }
    let (mut m, _) = exported("m-1", &format!("note {}", low_token()));
    m.memory_id = mixed_token();
    assert_eq!(
        classify_mapped(&m),
        RowVerdict::AutoRefuse(SECRET_SHAPED_ID)
    );
}

/// Why (#8624 items 7-9): through the real `HandleSink`, a redacted row lands
/// with its redacted text, a carve-out row lands verbatim tagged
/// `kuzu-review:override`, and every other row stays refused; a second run
/// writes nothing.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redacted_and_override_rows_land_through_the_handle_sink() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sink = HandleSink {
        handle: palace(tmp.path(), "kuzu-review-admit"),
    };
    let ledger = ledger_of(&review_entries());
    let export = export_of(&review_fixture());
    let c = run_plan_with(&export, STORE, Target::Write(&sink), false, Some(&ledger)).await;
    assert_eq!((c.new_memories, c.failed_writes), (3, 0), "{c:?}");
    assert_eq!((c.review_admitted, c.review_overrides), (1, 1));
    let refusals: Vec<(&str, usize)> = c.review_refusals.iter().map(|(k, n)| (*k, *n)).collect();
    assert_eq!(
        refusals,
        [
            (reason::AUTO_REFUSE, 1),
            (reason::NOT_CARVE_OUT, 1),
            (reason::STILL_FLAGGED, 1)
        ]
    );
    assert_eq!(c.refused_ids, ["m-4", "m-5", "m-6"]);
    let drawers = sink.drawers();
    let by_id = |id: &str| {
        drawers
            .iter()
            .find(|d| {
                d.tags
                    .iter()
                    .any(|t| t == &format!("source:kuzu-memory/{id}"))
            })
            .unwrap_or_else(|| panic!("drawer {id}"))
            .clone()
    };
    let redacted = by_id("m-2");
    assert_eq!(
        redacted.content(),
        "deploy key [REDACTED:MIXED_CASE_ALNUM] rotated"
    );
    assert!(!redacted.tags.iter().any(|t| t == OVERRIDE_TAG));
    let verbatim = by_id("m-3");
    assert_eq!(verbatim.content(), format!("label {}", low_token()));
    assert!(
        verbatim.tags.iter().any(|t| t == OVERRIDE_TAG),
        "{:?}",
        verbatim.tags
    );
    let all: String = drawers.iter().map(|d| d.content().to_string()).collect();
    for tok in [
        high_token(),
        threshold_token(),
        threshold_b64_token(),
        mixed_token(),
        provider_token(),
    ] {
        assert!(
            leaked_windows(&tok, &all).is_empty(),
            "a refused token reached a drawer"
        );
    }
    let again = run_plan_with(&export, STORE, Target::Write(&sink), false, Some(&ledger)).await;
    assert_eq!((again.new_memories, again.unchanged), (0, 3));
    assert_eq!(sink.drawers().len(), 3);
}

/// Why (#8624 rule 5 — fail-open, end to end): a sealed ledger holding a cut
/// line aborts the import before any store is read, and so does a ledger
/// with no seal; the error names the line.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_ledger_aborts_the_import_before_any_store_is_read() {
    let tmp = tempfile::tempdir().expect("tmp");
    let store = store_on_disk(tmp.path(), "proj");
    let data_root = tmp.path().join("data");
    let daemon = FakeDaemon(None);
    let dir = tmp.path().join("review");
    std::fs::create_dir_all(&dir).expect("dir");
    let ledger_path = dir.join(LEDGER_FILE);
    let mut bytes = ledger_bytes(&review_entries()[..1]);
    let cut = serde_json::to_string(&review_entries()[1]).expect("json");
    bytes.extend(format!("{}\n", &cut[..cut.len() - 9]).as_bytes());
    std::fs::write(&ledger_path, &bytes).expect("ledger");
    let mut args = args_for(&store, "kuzu-review-abort", false);
    args.review_ledger = Some(ledger_path.clone());

    let runner = FakeRunner::writing(&review_fixture());
    let err = run_import_in(
        &args,
        std::slice::from_ref(&store),
        &env(&runner, &daemon, &data_root),
        None,
    )
    .await
    .expect_err("an unsealed ledger");
    assert!(err.to_string().contains("not sealed"), "{err}");
    assert!(runner.temp_dir().is_none(), "no store was read");

    let seal =
        json!({"schema": 1, "ledger_sha256": sha256_hex(&bytes), "entries": 2, "sealed_at": "t"});
    std::fs::write(dir.join(SEAL_FILE), format!("{seal}\n")).expect("seal");
    let runner = FakeRunner::writing(&review_fixture());
    let err = run_import_in(&args, &[store], &env(&runner, &daemon, &data_root), None)
        .await
        .expect_err("a cut line");
    assert!(
        matches!(&err, KuzuImportError::Io(m) if m.contains("line 2")),
        "{err}"
    );
    assert!(runner.temp_dir().is_none(), "no store was read");
    assert!(!data_root.exists(), "no palace was opened");
}
