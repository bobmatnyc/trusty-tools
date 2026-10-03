//! `# Context:` diff-preamble consumption (#8654).
//!
//! Why: the canary sentence in duettoresearch/code-intelligence#5906 reached
//! the diff as a `# Context:` preamble and was discarded as an unparsed
//! section. These tests pin where the preamble ends and where its text goes.
//! What: pure calls to `split_context_preamble` / `consume_context_preamble`.
//! Test: this module IS the tests.

use super::{cap_caller_context, consume_context_preamble, split_context_preamble};
use crate::pipeline::runner::CallerContext;

const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n\
                    @@ -1 +1 @@\n-old\n+new\n";

#[test]
fn context_preamble_is_split_at_the_first_file_header() {
    let raw = format!("# Context: PR description CANARY-8654\nsecond line\n{DIFF}");
    let (text, rest) = split_context_preamble(&raw).expect("marked preamble splits");
    assert_eq!(text, "PR description CANARY-8654\nsecond line");
    assert_eq!(rest, DIFF);

    // A `---`/`+++` pair with no `diff --git` line also ends the preamble.
    let plain = "# Context: why\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
    let (text, rest) = split_context_preamble(plain).expect("header pair ends it");
    assert_eq!(text, "why");
    assert!(rest.starts_with("--- a/x\n"), "{rest:?}");
}

#[test]
fn an_unmarked_preamble_is_left_in_the_diff() {
    let raw = format!("From 1234 Mon Sep 17 00:00:00 2001\nSubject: x\n{DIFF}");
    assert_eq!(split_context_preamble(&raw), None);
    let mut caller = CallerContext::default();
    assert_eq!(consume_context_preamble(raw.clone(), &mut caller), raw);
    assert_eq!(caller.pr_description, None);
}

#[test]
fn a_preamble_with_no_diff_after_it_is_left_alone() {
    let raw = "# Context: nothing follows\nmore prose\n".to_string();
    let mut caller = CallerContext::default();
    assert_eq!(consume_context_preamble(raw.clone(), &mut caller), raw);
    assert_eq!(caller.pr_description, None);
}

#[test]
fn consumed_preamble_reaches_the_pr_description() {
    let raw = format!("# Context: PR description CANARY-8654\n{DIFF}");
    let mut caller = CallerContext::default();
    let rest = consume_context_preamble(raw, &mut caller);
    assert_eq!(rest, DIFF, "the diff loses only the preamble");
    assert_eq!(
        caller.pr_description.as_deref(),
        Some("PR description CANARY-8654")
    );
}

#[test]
fn preamble_is_appended_after_a_caller_supplied_description() {
    let mut caller = CallerContext {
        pr_description: Some("from the flag".into()),
        ..CallerContext::default()
    };
    consume_context_preamble(format!("# Context: from the diff\n{DIFF}"), &mut caller);
    assert_eq!(
        caller.pr_description.as_deref(),
        Some("from the flag\n\nfrom the diff")
    );

    // The same text twice is not duplicated.
    consume_context_preamble(format!("# Context: from the diff\n{DIFF}"), &mut caller);
    assert_eq!(
        caller.pr_description.as_deref(),
        Some("from the flag\n\nfrom the diff")
    );
}

/// Each field is cut at `max` characters (not bytes — the multi-byte `é`
/// never splits) with a marker naming what was omitted; a field within the
/// cap, and an absent one, are untouched.
#[test]
fn caller_context_fields_are_capped_with_a_visible_marker() {
    let mut caller = CallerContext {
        pr_description: Some("ééééé-tail".into()),
        pr_discussion: Some("short".into()),
        referenced_code: None,
    };
    cap_caller_context(&mut caller, 5);
    let desc = caller.pr_description.as_deref().unwrap_or_default();
    assert!(
        desc.starts_with("ééééé\n[... truncated: 5 more characters"),
        "{desc}"
    );
    assert!(!desc.contains("-tail"), "{desc}");
    assert_eq!(caller.pr_discussion.as_deref(), Some("short"));
    assert_eq!(caller.referenced_code, None);
}
