//! Unit tests for the phase-PR title tag (#9571).

use super::{TitleDecision, conventional_prefix_len, decide, phase_tag};
use crate::commands::pr::metadata::{RefKind, RefsIssue, RefsLookup};

/// A readable issue with `title`.
fn issue(title: &str) -> RefsIssue {
    RefsIssue {
        number: 9572,
        kind: RefKind::Issue,
        milestone: None,
        projects: Vec::new(),
        title: title.to_string(),
    }
}

/// The phase grammar `render.rs` parses, which had no direct table test.
#[test]
fn pr_9571_phase_title_grammar() {
    let cases = [
        ("[EPIC_12 PHASE_2] wire it", Some("[EPIC_12 PHASE_2]")),
        // The number is an identifier; the tag is rebuilt canonically.
        ("[EPIC_12 PHASE_02] wire it", Some("[EPIC_12 PHASE_2]")),
        ("[EPIC_12 PHASE_2b] wire it", None),
        ("[epic_12 phase_2] wire it", None),
        ("[EPIC 12] the outcome", None),
        ("[EPIC] the outcome", None),
        ("fix: no tag at all", None),
    ];
    for (title, want) in cases {
        assert_eq!(phase_tag(title).as_deref(), want, "{title}");
    }
}

/// The `cliff.toml` grammar `^[a-z]+(\(.+\))?!?: `, as a prefix length.
#[test]
fn pr_9571_conventional_prefix_grammar() {
    let cases = [
        ("feat: add X", Some("feat: ")),
        ("feat(trusty-mpm): add X", Some("feat(trusty-mpm): ")),
        ("feat(x)!: add X", Some("feat(x)!: ")),
        ("feat!: add X", Some("feat!: ")),
        // The shortest prefix wins, so a later `): ` never moves the tag.
        ("fix(a): handle (b): c", Some("fix(a): ")),
        ("feat(): add X", None),
        ("Feat: add X", None),
        ("feat:add X", None),
        ("Add X", None),
        ("", None),
    ];
    for (title, want) in cases {
        let got = conventional_prefix_len(title).map(|n| &title[..n]);
        assert_eq!(got, want, "{title}");
    }
}

/// Every [`decide`] arm, including the two warnings `tests.rs` cannot see.
#[test]
fn pr_9571_decide_covers_every_arm() {
    let phase = issue("[EPIC_12 PHASE_2] wire it");
    let found = RefsLookup::Found(&phase);
    assert_eq!(
        decide("feat(x)!: add X", &found),
        TitleDecision::Retitle("feat(x)!: [EPIC_12 PHASE_2] add X".to_string())
    );
    assert_eq!(
        decide("feat(x): [EPIC_9 PHASE_1] add X", &found),
        TitleDecision::Unchanged
    );
    assert_eq!(
        decide("feat(x): add X", &RefsLookup::Absent),
        TitleDecision::Unchanged
    );
    let tracker = issue("[EPIC 12] outcome");
    assert_eq!(
        decide("feat(x): add X", &RefsLookup::Found(&tracker)),
        TitleDecision::Unchanged
    );
    assert!(matches!(decide("Add X", &found), TitleDecision::Warn(_)));

    // 256 characters exactly is allowed; one more is not.
    let fits = format!("feat: {}", "a".repeat(256 - 6 - 18));
    assert!(matches!(decide(&fits, &found), TitleDecision::Retitle(t) if t.chars().count() == 256));
    let over = format!("feat: {}", "a".repeat(256 - 6 - 17));
    assert!(matches!(decide(&over, &found), TitleDecision::Warn(_)));

    let unread = RefsLookup::Unreadable(9572);
    assert!(
        matches!(decide("feat(x): add X", &unread), TitleDecision::Unknown(m) if m.starts_with("title")),
    );
    assert_eq!(decide("Add X", &unread), TitleDecision::Unchanged);
    assert_eq!(
        decide("feat(x): [EPIC_12 PHASE_2] add X", &unread),
        TitleDecision::Unchanged
    );
}
