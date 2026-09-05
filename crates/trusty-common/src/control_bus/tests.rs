//! Coverage for the `control_bus` event types and the types-only boundary.
//!
//! Why: The types moved here from `trusty-agents-common` (#6846) are a wire
//!      contract between separate processes, so their serde shapes need
//!      assertions that live beside the definitions rather than in the crate
//!      they left. The boundary itself also needs a gate: the owner ruling that
//!      put the bus in trusty-console only holds as long as nothing grows a
//!      channel back into this module.
//! What: Serde round-trips for each type and tag shape, the `Filter` matrix,
//!       and `control_bus_declares_no_transport`, which reads this module's own
//!       sources at compile time and fails on any transport spelling.
//! Test: this file.

use super::*;
use serde_json::json;

fn sample_lifecycle() -> LifecycleEvent {
    LifecycleEvent::PmThinking {
        session_id: "s1".into(),
        text: "considering options".into(),
    }
}

fn envelope(payload: HarnessPayload, session: Option<&str>) -> HarnessEvent {
    HarnessEvent {
        source: HarnessSource::Agents,
        session: session.map(str::to_string),
        seq: 0,
        at: chrono::Utc::now(),
        payload,
    }
}

// ---- HarnessSource ----

#[test]
fn harness_source_round_trips() {
    for (src, tag) in [
        (HarnessSource::Agents, "\"agents\""),
        (HarnessSource::Mpm, "\"mpm\""),
        (HarnessSource::Code, "\"code\""),
    ] {
        let s = serde_json::to_string(&src).expect("serialize source");
        assert_eq!(s, tag);
        let back: HarnessSource = serde_json::from_str(&s).expect("deserialize source");
        assert_eq!(back, src);
    }
}

// ---- LifecycleEvent ----

#[test]
fn lifecycle_event_serializes_with_type_tag() {
    let s = serde_json::to_string(&sample_lifecycle()).expect("serialize");
    assert!(s.contains("\"type\":\"pm_thinking\""), "{s}");
    assert!(s.contains("\"session_id\":\"s1\""), "{s}");
}

#[test]
fn lifecycle_session_id_returns_correct_field() {
    let ev = LifecycleEvent::AgentMessage {
        session_id: "abc".into(),
        agent: "python".into(),
        text: "hi".into(),
    };
    assert_eq!(ev.session_id(), Some("abc"));
}

#[test]
fn lifecycle_recap_round_trips() {
    let ev = LifecycleEvent::RecapGenerated {
        session_id: "s9".into(),
        summary: "did a thing".into(),
        table_rows: vec![("step".into(), "ok".into())],
    };
    let s = serde_json::to_string(&ev).expect("serialize recap");
    let back: LifecycleEvent = serde_json::from_str(&s).expect("deserialize recap");
    assert_eq!(back, ev);
}

// ---- HarnessPayload tag shapes ----

#[test]
fn payload_lifecycle_round_trips() {
    let p = HarnessPayload::Lifecycle(sample_lifecycle());
    let s = serde_json::to_string(&p).expect("serialize");
    assert!(s.contains("\"domain\":\"lifecycle\""), "{s}");
    assert!(s.contains("\"event\":{"), "{s}");
    assert!(s.contains("\"type\":\"pm_thinking\""), "{s}");
    let back: HarnessPayload = serde_json::from_str(&s).expect("deserialize");
    assert_eq!(back, p);
}

#[test]
fn payload_hook_round_trips() {
    let p = HarnessPayload::Hook {
        kind: "pre_tool_use".into(),
        data: json!({"tool": "bash", "ok": true}),
    };
    let s = serde_json::to_string(&p).expect("serialize");
    assert!(s.contains("\"domain\":\"hook\""), "{s}");
    assert!(s.contains("\"kind\":\"pre_tool_use\""), "{s}");
    let back: HarnessPayload = serde_json::from_str(&s).expect("deserialize");
    assert_eq!(back, p);
}

#[test]
fn payload_ping_round_trips() {
    let p = HarnessPayload::Ping;
    let s = serde_json::to_string(&p).expect("serialize");
    assert_eq!(s, "{\"domain\":\"ping\"}");
    let back: HarnessPayload = serde_json::from_str(&s).expect("deserialize");
    assert_eq!(back, p);
}

#[test]
fn payload_domain_matches_serde_tag() {
    assert_eq!(
        HarnessPayload::Lifecycle(sample_lifecycle()).domain(),
        "lifecycle"
    );
    assert_eq!(
        HarnessPayload::Hook {
            kind: "x".into(),
            data: json!(null)
        }
        .domain(),
        "hook"
    );
    assert_eq!(HarnessPayload::Ping.domain(), "ping");
}

// ---- HarnessEvent envelope ----

#[test]
fn harness_event_round_trips() {
    let ev = envelope(HarnessPayload::Ping, Some("sess-1"));
    let s = serde_json::to_string(&ev).expect("serialize");
    assert!(s.contains("\"source\":\"agents\""), "{s}");
    assert!(s.contains("\"session\":\"sess-1\""), "{s}");
    let back: HarnessEvent = serde_json::from_str(&s).expect("deserialize");
    assert_eq!(back, ev);
}

#[test]
fn harness_event_omits_none_session() {
    let ev = envelope(HarnessPayload::Ping, None);
    let s = serde_json::to_string(&ev).expect("serialize");
    assert!(!s.contains("session"), "session should be omitted: {s}");
}

// ---- Filter matrix ----

#[test]
fn filter_default_matches_all() {
    let f = Filter::default();
    assert!(f.matches(&envelope(HarnessPayload::Ping, None)));
    assert!(f.matches(&envelope(
        HarnessPayload::Lifecycle(sample_lifecycle()),
        Some("x")
    )));
}

#[test]
fn filter_by_source() {
    let f = Filter {
        source: Some(HarnessSource::Mpm),
        ..Default::default()
    };
    let mut ev = envelope(HarnessPayload::Ping, None);
    ev.source = HarnessSource::Mpm;
    assert!(f.matches(&ev));
    ev.source = HarnessSource::Agents;
    assert!(!f.matches(&ev));
}

#[test]
fn filter_by_session() {
    let f = Filter {
        session: Some("sess-7".into()),
        ..Default::default()
    };
    assert!(f.matches(&envelope(HarnessPayload::Ping, Some("sess-7"))));
    assert!(!f.matches(&envelope(HarnessPayload::Ping, Some("other"))));
    // An event with no session never matches a session constraint.
    assert!(!f.matches(&envelope(HarnessPayload::Ping, None)));
}

#[test]
fn filter_by_domain() {
    let f = Filter {
        domains: Some(vec!["hook", "ping"]),
        ..Default::default()
    };
    assert!(f.matches(&envelope(HarnessPayload::Ping, None)));
    assert!(f.matches(&envelope(
        HarnessPayload::Hook {
            kind: "k".into(),
            data: json!({})
        },
        None
    )));
    assert!(!f.matches(&envelope(
        HarnessPayload::Lifecycle(sample_lifecycle()),
        Some("x")
    )));
}

#[test]
fn filter_combination() {
    let f = Filter {
        source: Some(HarnessSource::Code),
        session: Some("s".into()),
        domains: Some(vec!["lifecycle"]),
    };
    let mut ev = envelope(HarnessPayload::Lifecycle(sample_lifecycle()), Some("s"));
    ev.source = HarnessSource::Code;
    assert!(f.matches(&ev));

    // Wrong source fails the conjunction even though session+domain match.
    ev.source = HarnessSource::Mpm;
    assert!(!f.matches(&ev));
}

// ---- types-only boundary ----

/// Every source file in this module, paired with its name for the failure
/// message. `include_str!` resolves relative to this file, so the check reads
/// the real shipped text rather than a list someone has to remember to update.
const MODULE_SOURCES: &[(&str, &str)] = &[
    ("control_bus/mod.rs", include_str!("mod.rs")),
    ("control_bus/lifecycle.rs", include_str!("lifecycle.rs")),
    ("control_bus/envelope.rs", include_str!("envelope.rs")),
    ("control_bus/filter.rs", include_str!("filter.rs")),
    ("control_bus/tests.rs", include_str!("tests.rs")),
];

/// Transport spellings that must never appear in this module.
///
/// Each needle is assembled by `concat!` so the literal it forms is absent from
/// this file's own source text — that is what lets the scan include `tests.rs`
/// itself instead of carving out an unchecked hole.
const FORBIDDEN_SUBSTRINGS: &[&str] = &[
    concat!("broadcast", "::"),
    concat!("Once", "Lock"),
    concat!("tokio", "::", "sync"),
    concat!("lazy_", "static!"),
    concat!("once_", "cell"),
];

/// Strips a leading visibility qualifier (`pub`, `pub(crate)`, `pub(super)`,
/// `pub(in ...)`) and the whitespace after it, so a static-declaration check
/// can anchor on `static` regardless of which visibility form precedes it.
///
/// Why: The transport scan below needs `static` at the true start of an item
///      declaration; without stripping visibility first, `pub(crate) static`
///      and `pub(in crate::foo) static` slipped past a scan that only knew
///      about bare `static ` and `pub static ` (#6846 review note).
/// What: Removes one `pub` token, and — when followed by a parenthesized
///       qualifier — the balanced `(...)` after it, then trims the remaining
///       leading whitespace. A line with no `pub` prefix passes through
///       unchanged.
/// Test: `static_scan_catches_every_visibility_form`.
fn strip_leading_pub(trimmed: &str) -> &str {
    let Some(rest) = trimmed.strip_prefix("pub") else {
        return trimmed;
    };
    if let Some(after_paren) = rest.strip_prefix('(') {
        // `pub(crate)`, `pub(super)`, `pub(in path::to::mod)` — the
        // qualifier never itself contains `)`, so the first one closes it.
        match after_paren.find(')') {
            Some(close) => after_paren[close + 1..].trim_start(),
            None => rest, // malformed `pub(` with no close — leave as-is
        }
    } else if rest.starts_with(char::is_whitespace) || rest.is_empty() {
        rest.trim_start()
    } else {
        // e.g. "public" — not actually the `pub` keyword.
        trimmed
    }
}

/// The `control_bus` module carries types and nothing that moves them.
///
/// Why: The owner ruling for #6846 puts the one event bus in trusty-console and
///      leaves `trusty-common` holding only the shared types. A grep proves that
///      on the day it is run and nothing afterwards; this test makes the
///      boundary fail the build the moment a channel, a process-global sender,
///      or a mutable global is added back.
/// What: Scans every source file of this module for a channel or global-state
///       spelling, and for any item-position `static` declaration (`static`
///       and `static mut`, under any visibility — `pub`, `pub(crate)`,
///       `pub(super)`, `pub(in ...)`, or none). The substring needles are
///       `concat!`-assembled so scanning this file does not match the needle
///       list itself; `&'static str` in a type position is not an item
///       declaration, so the `static` check is line-anchored (after stripping
///       a leading visibility qualifier) rather than a substring search.
/// Test: this test itself, plus the visibility-form negative control in
///       `static_scan_catches_every_visibility_form`.
#[test]
fn control_bus_declares_no_transport() {
    for (name, src) in MODULE_SOURCES {
        for needle in FORBIDDEN_SUBSTRINGS {
            assert!(
                !src.contains(needle),
                "{name} contains `{needle}`: control_bus holds event TYPES only \
                 — the bus lives in trusty-console (#6846)"
            );
        }

        for (idx, line) in src.lines().enumerate() {
            let candidate = strip_leading_pub(line.trim_start());
            assert!(
                !(candidate.starts_with("static ") || candidate.starts_with("static mut ")),
                "{}:{} declares a global `static`: control_bus holds event TYPES \
                 only — no global state (#6846)\n  {line}",
                name,
                idx + 1
            );
        }
    }
}

/// Negative control for the visibility stripping in
/// `control_bus_declares_no_transport`.
///
/// Why: The scan above is only as good as its line-anchoring. A prior version
///      caught bare `static ` and `pub static ` but missed `pub(crate) static`
///      and `static mut` — this test proves the fix against tiny inline
///      fixtures rather than trusting the real module sources to happen to
///      cover every visibility form.
/// What: Runs the same detection the scan above uses — strip a leading `pub`
///       qualifier, then check for a `static ` or `static mut ` prefix —
///       against three one-line fixtures: a `pub(crate) static` declaration
///       (must be caught), a `static mut` declaration with no visibility
///       (must be caught), and a `&'static` reference in a `let` binding
///       (must NOT be caught).
/// Test: this test itself.
#[test]
fn static_scan_catches_every_visibility_form() {
    let is_static_declaration = |line: &str| -> bool {
        let candidate = strip_leading_pub(line.trim_start());
        candidate.starts_with("static ") || candidate.starts_with("static mut ")
    };

    assert!(
        is_static_declaration("pub(crate) static X: u8 = 0;"),
        "`pub(crate) static` must be detected as a static declaration"
    );
    assert!(
        is_static_declaration("static mut Y: u8 = 0;"),
        "`static mut` with no visibility qualifier must be detected"
    );
    assert!(
        !is_static_declaration("let s: &'static str = \"\";"),
        "`&'static` in a type position must not be flagged as a static declaration"
    );
}

/// The scan above covers every file the module actually ships.
///
/// Why: `MODULE_SOURCES` is a hand-written list, and a new submodule added
///      without a row would be silently unscanned — the fail-open mode that
///      makes a guard worthless. Counting the module's declarations against the
///      list turns that omission into a failure.
/// What: Reads `mod.rs` for its `mod <name>;` declarations and asserts each one
///       has a `MODULE_SOURCES` row, and that the list also covers `mod.rs` and
///       this file.
/// Test: this test itself.
#[test]
fn module_source_scan_covers_every_submodule() {
    let mod_rs = include_str!("mod.rs");

    let declared: Vec<&str> = mod_rs
        .lines()
        .map(str::trim)
        .filter_map(|l| {
            l.strip_prefix("mod ")
                .or_else(|| l.strip_prefix("pub mod "))
        })
        .filter_map(|rest| rest.strip_suffix(';'))
        .collect();

    assert!(
        !declared.is_empty(),
        "found no `mod` declarations in control_bus/mod.rs — the parser above is \
         out of date, which would make the transport scan vacuous"
    );

    for name in &declared {
        let expected = format!("control_bus/{name}.rs");
        assert!(
            MODULE_SOURCES.iter().any(|(n, _)| *n == expected),
            "control_bus/mod.rs declares `mod {name};` but MODULE_SOURCES has no \
             row for {expected} — add one so the transport scan covers it"
        );
    }

    // `mod.rs` and `tests.rs` are not `mod` declarations, so assert them directly.
    for expected in ["control_bus/mod.rs", "control_bus/tests.rs"] {
        assert!(
            MODULE_SOURCES.iter().any(|(n, _)| *n == expected),
            "MODULE_SOURCES is missing {expected}"
        );
    }
}
