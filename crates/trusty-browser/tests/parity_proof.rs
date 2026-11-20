//! Pairing-proof parity against `tests/fixtures/proof_vectors.json` (Python 0.7.0).

mod common;

use common::{items, json, s};
use trusty_browser::proof::{self, Hello, Role};

#[test]
fn proof_vectors_match_python_byte_for_byte() {
    let vectors = json("proof_vectors.json");
    assert_eq!(items(&vectors).len(), 4);
    for v in items(&vectors) {
        let role = match s(v, "role") {
            "broker" => Role::Broker,
            "extension" => Role::Extension,
            other => panic!("role {other}"),
        };
        let h = v.get("hello").unwrap_or_else(|| panic!("hello"));
        let hello = Hello {
            profile: s(h, "profile"),
            email: s(h, "email"),
            incarnation: s(h, "incarnation"),
            nonce: s(h, "nonce"),
        };
        let server = s(v, "server_nonce");
        let bytes = proof::transcript(role, &hello, server);
        assert_eq!(
            hex::encode(&bytes),
            s(v, "transcript_hex"),
            "{} {}",
            s(v, "role"),
            hello.email
        );
        let mac = proof::pairing_proof(s(v, "key"), role, &hello, server);
        assert_eq!(mac.as_deref().ok(), Some(s(v, "mac")));
        assert!(proof::proofs_equal(s(v, "mac"), &mac.unwrap_or_default()));
    }
}
