//! Unit tests for the author-intent rule and carry-over gate (#10758).

use super::*;
use crate::models::Effort;
use crate::pipeline::withheld_contract::reason_class;

const LEGACY: &str = "var x = getData(); // TODO cleanup";
const NEW_BUG: &str = "let total = amounts.iter().sum::<u32>();";

fn diff() -> String {
    format!(
        "diff --git a/old/a.js b/old/a.js\n--- a/old/a.js\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-  {LEGACY}\n\
         diff --git a/new/a.js b/new/a.js\n--- /dev/null\n+++ b/new/a.js\n@@ -0,0 +1,2 @@\n+  {LEGACY}\n+  {NEW_BUG}\n"
    )
}

fn finding(quote: &str, severity: Severity) -> Finding {
    let mut f = Finding::new("new/a.js", "style", "body", "", 0.5, Effort::Low);
    f.quoted_snippet = Some(quote.to_string());
    f.severity = Some(severity);
    f
}

fn gate(intent: Option<&str>, findings: &mut Vec<Finding>) -> Vec<WithheldFinding> {
    let mut withheld = Vec::new();
    let n = withhold_carried_over(intent, &diff(), findings, &mut withheld);
    assert_eq!(n, withheld.len());
    withheld
}

#[test]
fn as_is_markers_are_recognised() {
    assert!(declares_as_is_migration("As-is legacy migration"));
    assert!(declares_as_is_migration(
        "verbatim move, no behavior change"
    ));
    assert!(!declares_as_is_migration("refactor the billing module"));
}

#[test]
fn intent_section_renders_the_rule() {
    let s = intent_section(Some("as-is legacy migration"));
    assert!(
        s.starts_with("## Author Intent\n\nas-is legacy migration"),
        "{s}"
    );
    assert!(s.contains("do NOT report legacy style"), "{s}");
    assert!(s.contains("Still report every correctness"), "{s}");
}

#[test]
fn blank_intent_renders_nothing() {
    assert_eq!(intent_section(None), "");
    assert_eq!(intent_section(Some("  ")), "");
}

#[test]
fn with_intent_section_prepends_only_when_set() {
    assert_eq!(with_intent_section(None, "S".into()), "S");
    assert!(with_intent_section(Some("as-is"), "S".into()).ends_with("\nS"));
    assert!(with_intent_section(Some("as-is"), String::new()).starts_with("## Author Intent"));
}

#[test]
fn carried_over_style_finding_is_withheld() {
    let mut findings = vec![finding(LEGACY, Severity::Low)];
    let withheld = gate(Some("as-is legacy migration"), &mut findings);
    assert!(findings.is_empty());
    assert_eq!(reason_class(&withheld[0].reason), "as_is_carryover");
}

#[test]
fn introduced_bug_on_new_code_is_kept() {
    let mut findings = vec![finding(NEW_BUG, Severity::Medium)];
    assert!(gate(Some("as-is legacy migration"), &mut findings).is_empty());
    assert_eq!(findings.len(), 1);
}

#[test]
fn high_severity_carry_over_is_kept() {
    let mut findings = vec![finding(LEGACY, Severity::High)];
    assert!(gate(Some("as-is legacy migration"), &mut findings).is_empty());
    assert_eq!(findings.len(), 1);
}

#[test]
fn no_intent_withholds_nothing() {
    for intent in [None, Some("refactor billing")] {
        let mut findings = vec![finding(LEGACY, Severity::Low)];
        assert!(gate(intent, &mut findings).is_empty());
        assert_eq!(findings.len(), 1);
    }
}

// #10758: negated or loose wording must not switch the gate on.
#[test]
fn negated_or_loose_intent_is_not_as_is() {
    for intent in [
        "this is not an as-is port",
        "rewrite, not verbatim",
        "public API unchanged",
        "use the data as is",
    ] {
        assert!(!declares_as_is_migration(intent), "{intent}");
    }
    assert!(declares_as_is_migration("verbatim port"));
}

// #10758: a quote only on the removed side (moved into new context), a quote
// whose repeat count the added side lacks, and a trivial quote are all kept.
#[test]
fn changed_context_or_trivial_quote_is_kept() {
    let diff = "--- a/x.js\n+++ b/x.js\n@@ -1,4 +1,4 @@\n\
                -  doWork(items);\n-  retryOnce(items);\n-  retryOnce(items);\n-  }\n\
                +  if (flag) { doWork(items); }\n+  retryOnce(items);\n+  }\n";
    for quote in [
        "doWork(items);",
        "retryOnce(items);\nretryOnce(items);",
        "}",
    ] {
        let mut findings = vec![finding(quote, Severity::Low)];
        let mut withheld = Vec::new();
        withhold_carried_over(Some("as-is migration"), diff, &mut findings, &mut withheld);
        assert!(withheld.is_empty(), "{quote}");
        assert_eq!(findings.len(), 1);
    }
}

// #10758: an unset severity is never withheld.
#[test]
fn unset_severity_is_kept() {
    let mut findings = vec![finding(LEGACY, Severity::Low)];
    findings[0].severity = None;
    assert!(gate(Some("as-is legacy migration"), &mut findings).is_empty());
    assert_eq!(findings.len(), 1);
}
