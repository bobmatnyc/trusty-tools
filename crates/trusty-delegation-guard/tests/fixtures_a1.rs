//! The A1 synthetic fixtures: nine packets, nine labels, separate directories.
#![cfg(feature = "types")]

mod common;

use common::*;
use trusty_delegation_guard::DgEvent;

#[test]
fn fixture_labels_match_a1_counts() {
    let labels = labels();
    let packets = packets();
    assert_eq!(labels.len(), 9);
    assert_eq!(packets.len(), 9);
    let count = |v: &str| labels.iter().filter(|l| l.expected_verdict == v).count();
    assert_eq!(
        (
            count("allow"),
            count("revise"),
            count("insufficient_evidence")
        ),
        (6, 2, 1)
    );
    for (p, l) in packets.iter().zip(&labels) {
        assert_eq!(p.case_id, l.case_id, "packet and label sets differ");
        for id in &l.binding_rule_ids {
            assert!(
                p.rules.iter().any(|r| &r.rule_id == id),
                "{}: label cites a rule the packet lacks",
                l.case_id
            );
        }
    }
}

#[test]
fn every_packet_builds_a_valid_event() {
    for p in packets() {
        let event = packet_event(&p);
        let back = DgEvent::from_json(&event.to_json().expect("json")).expect("parse");
        assert_eq!(back, event, "{}", p.case_id);
        assert_eq!(event.project_id.as_str(), p.project_id);
    }
}

/// A1: no label reaches the event request. The provider-body half of this
/// test lands with the request builder (PR2).
#[test]
fn request_carries_no_label() {
    let labels = labels();
    for p in packets() {
        let json = packet_event(&p).to_json().expect("json");
        for key in ["expected_verdict", "binding_rule_ids", "label_note"] {
            assert!(
                !json.contains(key),
                "{}: label key {key} in request",
                p.case_id
            );
        }
        for l in &labels {
            assert!(
                !json.contains(&l.label_note),
                "{}: label text in request",
                p.case_id
            );
        }
    }
}
