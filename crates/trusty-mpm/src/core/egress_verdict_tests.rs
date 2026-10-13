//! Tests for the egress verdict wire contract (#8454 VC).

use serde_json::json;

use super::channels_readiness::{HostBranch, NotReady};
use super::egress_verdict::{Unserved, VerdictReply};

/// 🔴 #8454 VC: each reply has one wire shape, and an unknown `answer` or
/// `why` does not parse, so the hook can never read it as a verdict.
#[test]
fn verdict_reply_wire_shape_is_closed() {
    let not_ready = VerdictReply::NotReady {
        slug: "egress-policy-not-ready".into(),
        not_ready: NotReady::Host {
            branch: HostBranch::Missing,
            error: "Missing".into(),
        },
        fingerprint: 7,
        age_ms: 12,
    };
    let cases = [
        (
            VerdictReply::Ready {
                fingerprint: 7,
                age_ms: 12,
            },
            json!({"answer": "ready", "fingerprint": 7, "age_ms": 12}),
        ),
        (
            not_ready,
            json!({"answer": "not_ready", "slug": "egress-policy-not-ready",
                   "not_ready": {"Host": {"branch": "Missing", "error": "Missing"}},
                   "fingerprint": 7, "age_ms": 12}),
        ),
        (
            VerdictReply::Unserved {
                why: Unserved::NoFingerprint,
            },
            json!({"answer": "unserved", "why": "no_fingerprint"}),
        ),
    ];
    for (reply, wire) in cases {
        assert_eq!(serde_json::to_value(&reply).expect("serialize"), wire);
        let back: VerdictReply = serde_json::from_value(wire).expect("parse");
        assert_eq!(back, reply);
    }
    for bad in [
        json!({"answer": "allow", "fingerprint": 7, "age_ms": 1}),
        json!({"answer": "unserved", "why": "something_new"}),
        // #8454: the retired store-then-unserved reason is no longer a reply.
        json!({"answer": "unserved", "why": "fingerprint_changed"}),
        json!({"answer": "ready", "fingerprint": 7}),
        json!({"fingerprint": 7, "age_ms": 1}),
    ] {
        assert!(
            serde_json::from_value::<VerdictReply>(bad.clone()).is_err(),
            "{bad} must not parse"
        );
    }
}
