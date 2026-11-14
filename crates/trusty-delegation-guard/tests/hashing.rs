//! Canonical hashing and the dedupe key.
#![cfg(feature = "types")]

use trusty_delegation_guard::hash::{self, canonical_sha256};
use trusty_delegation_guard::{DedupeSet, Seen, Sha256Hex};

#[test]
fn canonical_hash_ignores_field_order() {
    let a: serde_json::Value =
        serde_json::from_str(r#"{"b":1,"a":{"y":[1,{"q":2,"p":3}],"x":"s"}}"#).expect("a");
    let b: serde_json::Value =
        serde_json::from_str(r#"{"a":{"x":"s","y":[1,{"p":3,"q":2}]},"b":1}"#).expect("b");
    assert_eq!(
        canonical_sha256(&a).expect("hash a"),
        canonical_sha256(&b).expect("hash b")
    );
    assert_eq!(
        hash::canonical_json(&a).expect("json"),
        r#"{"a":{"x":"s","y":[1,{"p":3,"q":2}]},"b":1}"#
    );
    // Array order is meaningful and must still change the hash.
    let c: serde_json::Value =
        serde_json::from_str(r#"{"a":{"x":"s","y":[{"p":3,"q":2},1]},"b":1}"#).expect("c");
    assert_ne!(
        canonical_sha256(&a).expect("hash a"),
        canonical_sha256(&c).expect("hash c")
    );
}

fn d(tag: &str) -> Sha256Hex {
    Sha256Hex::digest(tag.as_bytes())
}

#[test]
fn dedupe_key_changes_with_each_input() {
    let (action, rev, prompt, tree) = (d("action"), d("rev"), d("prompt"), d("tree"));
    let base = hash::dedupe_key("launch", &action, &rev, &prompt, Some(&tree));
    assert_eq!(
        base,
        hash::dedupe_key("launch", &action, &rev, &prompt, Some(&tree)),
        "same inputs must give the same key"
    );
    let variants = [
        hash::dedupe_key("contract", &action, &rev, &prompt, Some(&tree)),
        hash::dedupe_key("launch", &d("action2"), &rev, &prompt, Some(&tree)),
        hash::dedupe_key("launch", &action, &d("rev2"), &prompt, Some(&tree)),
        hash::dedupe_key("launch", &action, &rev, &d("prompt2"), Some(&tree)),
        hash::dedupe_key("launch", &action, &rev, &prompt, Some(&d("tree2"))),
        hash::dedupe_key("launch", &action, &rev, &prompt, None),
    ];
    for (i, v) in variants.iter().enumerate() {
        assert_ne!(v, &base, "changing input {i} left the key unchanged");
    }
    let expected = Sha256Hex::digest(format!("launch|{action}|{rev}|{prompt}|{tree}").as_bytes());
    assert_eq!(base, expected, "key must be sha256 of the |-joined parts");
}

#[test]
fn dedupe_set_detects_repeat_key() {
    let key = hash::dedupe_key("contract", &d("a"), &d("r"), &d("p"), None);
    let other = hash::dedupe_key("contract", &d("a"), &d("r2"), &d("p"), None);
    let mut set = DedupeSet::default();
    assert_eq!(set.observe(&key), Seen::First);
    assert_eq!(set.observe(&key), Seen::Duplicate);
    assert_eq!(set.observe(&other), Seen::First);
}

#[test]
fn sha256_hex_rejects_non_digests() {
    for bad in [
        "",
        "abc",
        &"A".repeat(64),
        &"g".repeat(64),
        &"a".repeat(63),
        &"a".repeat(65),
    ] {
        let json = serde_json::to_string(bad).expect("json");
        assert!(
            serde_json::from_str::<Sha256Hex>(&json).is_err(),
            "{bad:?} accepted"
        );
    }
    let good = serde_json::to_string(&"0a".repeat(32)).expect("json");
    assert!(serde_json::from_str::<Sha256Hex>(&good).is_ok());
}
