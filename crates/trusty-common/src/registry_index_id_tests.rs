//! Tests for [`super`]: the registry-derived index id (#10530).

use super::*;
use std::path::Path;

/// Why: pins the scheme so a silent change cannot re-point every registered id.
/// Test: itself.
#[test]
fn derivation_is_pinned_for_a_known_input() {
    // sha256("v2:bobmatnyc/trusty-tools") starts 0c7a4c7a.
    let id = derive_registry_index_id("trusty-tools", "bobmatnyc/trusty-tools");
    assert_eq!(id, "trusty-tools-0c7a4c7a");
}

/// Why: the id must not depend on the checkout path, the failure #4269 records.
/// Test: itself.
#[test]
fn same_key_gives_same_id_whatever_the_name_spelling_of_the_path() {
    let reg = RegisteredProject::new("trusty-tools", "bobmatnyc/trusty-tools");
    let a = registry_id_for_root(Path::new("/work/a/trusty-tools"), Some(&reg));
    let b = registry_id_for_root(Path::new("/elsewhere/moved"), Some(&reg));
    assert_eq!(a, b);
}

/// Why: two projects with one name still need two indexes.
/// Test: itself.
#[test]
fn different_keys_give_different_ids() {
    let a = derive_registry_index_id("app", "acme/app");
    let b = derive_registry_index_id("app", "local:0f0c8d2e-0000-4000-8000-000000000001");
    assert_ne!(a, b);
}

/// Why: the id is one URL path segment whatever the name holds.
/// Test: itself.
#[test]
fn id_is_one_url_safe_segment() {
    for name in ["My Project!", "", "---", "ÜberRepo", &"x".repeat(200)] {
        let id = derive_registry_index_id(name, "local:abc");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "{id}"
        );
        assert!(!id.starts_with('-'), "{id}");
    }
}

/// Why: a registered root must use the registry identity once adopted.
/// Test: itself.
#[test]
fn registry_id_for_root_prefers_the_registered_identity() {
    let reg = RegisteredProject::new("widget", "acme/widget");
    let id = registry_id_for_root(Path::new("/x/widget"), Some(&reg));
    assert_eq!(id, derive_registry_index_id("widget", "acme/widget"));
}

/// Why: an unregistered root keeps the id registration always created.
/// Test: itself.
#[test]
fn registry_id_for_root_falls_back_to_the_legacy_basename() {
    assert_eq!(registry_id_for_root(Path::new("/x/widget"), None), "widget");
}

/// Why: until trusty-code can look up a key, tm must not name an index that
/// trusty-code would not (a duplicate per project). Flip with the adoption.
/// Test: itself.
#[test]
fn index_id_for_root_stays_legacy_until_adoption() {
    const { assert!(!REGISTRY_IDS_ADOPTED) };
    let reg = RegisteredProject::new("renamed", "acme/widget");
    assert_eq!(
        index_id_for_root(Path::new("/x/widget"), Some(&reg)),
        "widget"
    );
    assert_eq!(index_id_for_root(Path::new("/x/widget"), None), "widget");
}
