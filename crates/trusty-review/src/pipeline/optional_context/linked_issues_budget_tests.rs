//! `fetch_linked_issues` budget tests: supplied docs kept, the fetched tail
//! dropped whole (#9197, B2b AC5-AC8, AC15, F10).
//!
//! Why: the Architect ruling keeps every supplied doc and drops the fetched
//! tail first; a wrong implementation would fetch a supplied number, spend
//! the 5-call budget on skipped refs, cut a fetched doc to fit, skip a big
//! doc to keep a small later one, or fetch when the section is already full.
//! What: drives [`apply_linked_issues`] through the parent module's `run`
//! and `fetch` helpers with sized bodies.
//! Test: this module.

use super::*;
use crate::config::constants::MAX_ISSUE_DOC_CHARS;

/// A body of exactly `len` characters that starts with `tag`.
fn sized(tag: &str, len: usize) -> String {
    format!("{tag} {}", "x".repeat(len - tag.len() - 1))
}

/// Supplied docs `#11`, `#12`, … with bodies of these sizes, tagged `S11`, ….
fn supplied(sizes: &[usize]) -> Vec<IssueDoc> {
    let tagged = sizes.iter().enumerate().map(|(i, len)| (i + 11, *len));
    tagged
        .map(|(n, len)| doc(&format!("#{n}"), &sized(&format!("S{n}"), len)))
        .collect()
}

/// A fetcher answering `#1`, `#2`, … with bodies of these sizes, tagged `F1`, ….
fn fetched(sizes: &[usize]) -> FakeFetcher {
    let numbered = sizes
        .iter()
        .enumerate()
        .map(|(i, len)| (i as u64 + 1, *len));
    numbered.fold(FakeFetcher::default(), |f, (n, len)| {
        f.doc(n, &sized(&format!("F{n}"), len))
    })
}

/// AC5, AC6 (amendment 8): a supplied number is never fetched, whatever its
/// body; supplied and self refs take none of the 5 calls.
#[tokio::test]
async fn supplied_blank_doc_suppresses_the_fetch_and_supplied_refs_take_no_budget() {
    let body = "Refs #1, #2, #3, #4, #5, #6, #7";
    for (text, shown) in [("", false), ("SUPPLIED_2", true)] {
        let request = OptionalContextRequest::default()
            .with_fetch_linked_issues(true)
            .with_issue_docs(vec![doc("#2", text)]);
        let seen = run(
            request,
            PrBody::Fetched(body),
            &github(99),
            fetched(&[9; 7]),
        )
        .await;
        assert_eq!(seen.calls, ["1", "3", "4", "5", "6"]);
        let two: Vec<_> = seen.row.items.iter().filter(|i| i.id == "#2").collect();
        assert_eq!(
            two.last().map(|i| i.detail.as_deref()),
            Some(Some("supplied"))
        );
        let over = item(&seen.row, "(over the 5-issue fetch limit)");
        assert_eq!(
            over.detail.as_deref(),
            Some("1 more same-repository refs are not fetched")
        );
        assert!(!seen.section.contains("F2 ") && seen.section.contains("SUPPLIED_2") == shown);
    }
}

/// AC7: supplied docs over the cap drop by the B2a rule; no fetch displaces one.
#[tokio::test]
async fn supplied_docs_over_the_cap_still_drop_by_the_existing_rule() {
    let docs = supplied(&[MAX_ISSUE_DOC_CHARS; 4]);
    let seen = fetch("Refs #1", Some(docs), fetched(&[10])).await;
    assert!(seen.calls.is_empty(), "the section is full");
    for tag in ["S11 ", "S12 ", "S13 "] {
        assert!(seen.section.contains(tag), "{tag}");
    }
    assert!(!seen.section.contains("S14 ") && !seen.section.contains("F1 "));
    assert_eq!(item(&seen.row, "#14").detail, Some(section_cap_reason()));
    // The linked issue is recorded, not silently dropped (AC8).
    assert_eq!(item(&seen.row, "#1").detail, Some(section_cap_reason()));
}

/// AC8 (amendment 7): supplied docs that fill the section exactly mean zero
/// calls and every candidate `omitted` with the cap reason; one free
/// character means the fetch runs.
#[tokio::test]
async fn fetch_is_skipped_when_supplied_fill_the_cap() {
    let full = supplied(&[MAX_ISSUE_DOC_CHARS; 3]);
    let seen = fetch("Refs #1, #2", Some(full), fetched(&[10, 10])).await;
    assert!(seen.calls.is_empty());
    for id in ["#1", "#2"] {
        assert_eq!(item(&seen.row, id).state, SourceState::Omitted);
        assert_eq!(item(&seen.row, id).detail, Some(section_cap_reason()));
    }
    let short = supplied(&[
        MAX_ISSUE_DOC_CHARS,
        MAX_ISSUE_DOC_CHARS,
        MAX_ISSUE_DOC_CHARS - 1,
    ]);
    let seen = fetch("Refs #1, #2", Some(short), fetched(&[10, 10])).await;
    assert_eq!(seen.calls, ["1", "2"]);
    assert_eq!(item(&seen.row, "#1").detail, Some(section_cap_reason()));
}

/// AC7: a fetched doc that does not fit is omitted whole, never cut, with
/// every later fetched doc, even one small enough to fit.
#[tokio::test]
async fn a_fetched_doc_that_does_not_fit_is_dropped_whole_with_every_later_one() {
    let docs = supplied(&[MAX_ISSUE_DOC_CHARS; 2]);
    let seen = fetch(
        "Refs #1, #2, #3",
        Some(docs),
        fetched(&[10_000, 20_000, 100]),
    )
    .await;
    assert_eq!(seen.calls, ["1", "2", "3"]);
    assert_eq!(item(&seen.row, "#1").state, SourceState::Used);
    let b = item(&seen.row, "#2");
    assert_eq!(
        (b.state, b.chars, b.chars_omitted),
        (SourceState::Omitted, 0, 20_000)
    );
    assert_eq!(item(&seen.row, "#3").state, SourceState::Omitted);
    assert!(seen.section.contains("F1 ") && !seen.section.contains("F2 "));
    assert!(!seen.section.contains("F3 ") && !seen.section.contains("truncated:"));
}

/// AC7 (amendment 7): supplied docs of 40,000 leave room, so the calls run,
/// and the fetched tail that does not fit is dropped whole after them.
#[tokio::test]
async fn a_fetched_tail_is_dropped_whole_after_a_call() {
    let docs = supplied(&[MAX_ISSUE_DOC_CHARS, MAX_ISSUE_DOC_CHARS, 8_000]);
    let seen = fetch("Refs #1, #2", Some(docs), fetched(&[10_000, 100])).await;
    assert_eq!(seen.calls, ["1", "2"]);
    for id in ["#1", "#2"] {
        assert_eq!(item(&seen.row, id).state, SourceState::Omitted, "{id}");
    }
    assert!(seen.section.contains("S13 ") && !seen.section.contains("F1 "));
    assert_eq!(seen.row.state, SourceState::Truncated);
}

/// AC7 (ruling Q3): item limits are per origin: 8 supplied and 5 fetched
/// render together under the shared character cap.
#[tokio::test]
async fn per_origin_limits_keep_eight_supplied_and_five_fetched() {
    let docs = supplied(&[3_000; 9]);
    let seen = fetch("Refs #1, #2, #3, #4, #5", Some(docs), fetched(&[2_000; 5])).await;
    assert_eq!(
        seen.section.matches("### Issue ").count(),
        13,
        "8 supplied + 5 fetched"
    );
    assert_eq!(
        item(&seen.row, "#19").detail.as_deref(),
        Some("over the 8-doc limit")
    );
    assert!(seen.section.contains("F5 "));
}

/// AC15: a fetched body past 16,000 characters is cut with the marker and
/// recorded `truncated`.
#[tokio::test]
async fn a_fetched_doc_over_the_doc_cap_is_truncated_with_its_marker() {
    let seen = fetch("Refs #1", None, fetched(&[20_000])).await;
    let one = item(&seen.row, "#1");
    assert_eq!(
        (one.state, one.chars, one.chars_omitted),
        (SourceState::Truncated, 16_000, 4_000)
    );
    let marker = "[... truncated: 4000 more characters omitted; issue #1 is capped at 16000";
    assert!(seen.section.contains(marker), "{}", seen.section);
}

/// F10: a repeated number is fetched once; a fetched doc that repeats a
/// supplied id renders once, with the supplied text.
#[tokio::test]
async fn a_duplicate_fetched_id_renders_once_and_supplied_wins() {
    let seen = fetch("Refs #3, acme/billing#3 and #3", None, fetched(&[5, 5, 5])).await;
    assert_eq!(seen.calls, ["3"]);
    assert_eq!(seen.section.matches("### Issue #3").count(), 1);
    let mut render = IssueRender::new();
    render.supplied(&[doc("#3", "SUPPLIED_3")]);
    render.fetched(&doc("#3", "FETCHED_3"));
    let (section, items) = render.finish();
    assert!(section.contains("SUPPLIED_3") && !section.contains("FETCHED_3"));
    assert_eq!(items[1].detail.as_deref(), Some("duplicate id"));
}
