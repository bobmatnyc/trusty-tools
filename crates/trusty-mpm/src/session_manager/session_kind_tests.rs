//! Tests for [`SessionKind`] and its place on `SessionRecord` (#8942).

use super::SessionKind;
use crate::session_manager::SessionRecord;

/// A minimal persisted record, with `extra` spliced in before the closing
/// brace (`""` for a pre-#8942 record).
fn record_json(extra: &str) -> String {
    format!(
        r#"{{"id":"6f1c2b9e-8f3a-4b7d-9c1e-2a3b4c5d6e7f","task":"t","tmux_name":"tm-x","cwd":"/tmp","state":"stopped","created_at":"2026-09-30T00:00:00Z"{extra}}}"#
    )
}

/// #8942: a record persisted before the field existed loads as `Ordinary`,
/// so no store rewrite is needed and nothing in the store becomes protected.
#[test]
fn legacy_record_without_kind_deserializes_as_ordinary() {
    let record: SessionRecord =
        serde_json::from_str(&record_json("")).expect("a pre-#8942 record must still load");
    assert_eq!(record.kind, SessionKind::Ordinary);
    assert!(
        record.is_auto_resumable(),
        "a legacy stopped record keeps its pre-#8942 auto-resume behavior"
    );
}

/// #8942 fail-closed: a kind a newer build wrote is `Unknown`, which is
/// protected and never auto-resumed — it never reads as `Ordinary`.
#[test]
fn an_unrecognised_kind_reads_as_unknown_and_is_protected() {
    let record: SessionRecord = serde_json::from_str(&record_json(r#","kind":"future_role""#))
        .expect("an unknown kind must not fail the whole store load");
    assert_eq!(record.kind, SessionKind::Unknown);
    assert!(record.kind.is_protected());
    assert!(!record.is_auto_resumable());
}

/// The wire tokens are snake_case and round-trip.
#[test]
fn session_kind_wire_tokens_round_trip() {
    for (kind, token) in [
        (SessionKind::Ordinary, "\"ordinary\""),
        (SessionKind::Supervisor, "\"supervisor\""),
        (SessionKind::SupervisorAux, "\"supervisor_aux\""),
    ] {
        assert_eq!(serde_json::to_string(&kind).expect("serialize"), token);
        let back: SessionKind = serde_json::from_str(token).expect("deserialize");
        assert_eq!(back, kind);
    }
}

#[test]
fn only_ordinary_is_unprotected() {
    assert!(!SessionKind::Ordinary.is_protected());
    for kind in [
        SessionKind::Supervisor,
        SessionKind::SupervisorAux,
        SessionKind::Unknown,
    ] {
        assert!(kind.is_protected(), "{kind:?}");
    }
}
