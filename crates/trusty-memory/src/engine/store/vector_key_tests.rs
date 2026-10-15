//! Tests for the `{uuid}#n` vector-key grammar (#9275).
//!
//! Test: this file IS the tests.

use super::*;

const ID: &str = "7d3f0c2a-1b4e-4c5d-9e6f-a0b1c2d3e4f1";
const OTHER: &str = "7d3f0c2a-1b4e-4c5d-9e6f-a0b1c2d3e4f2";

fn id() -> Uuid {
    Uuid::parse_str(ID).expect("fixture uuid")
}

#[test]
fn bare_uuid_is_chunk_zero() {
    assert_eq!(parse(ID), Ok((id(), 0)));
    assert_eq!(split(ID), Some((ID, 0)));
    assert_eq!(group_key(ID), ID);
}

#[test]
fn suffix_n_parses_to_chunk_n() {
    for n in [1u32, 2, 7, 10, 4096] {
        let key = format!("{ID}#{n}");
        assert_eq!(parse(&key), Ok((id(), n)), "{key}");
        assert_eq!(group_key(&key), ID, "{key}");
    }
}

#[test]
fn format_parse_round_trip_for_0_1_and_u32_max() {
    assert_eq!(format(ID, 0), ID, "chunk 0 is the bare key");
    assert_eq!(format(ID, 1), format!("{ID}#1"));
    for n in [0u32, 1, u32::MAX] {
        assert_eq!(parse(&format(ID, n)), Ok((id(), n)), "chunk {n}");
    }
}

#[test]
fn rejects_zero_suffix_leading_zero_empty_and_nested() {
    for suffix in [
        "#0",
        "#01",
        "#",
        "#x",
        "#1#2",
        "#-1",
        "#+1",
        "# 1",
        "#1 ",
        "#4294967296",
    ] {
        let key = format!("{ID}{suffix}");
        assert!(
            matches!(parse(&key), Err(KeyError::BadSuffix(_))),
            "{key:?}"
        );
        assert_eq!(split(&key), None, "{key:?}");
        assert_eq!(group_key(&key), key, "a bad row is a group of its own");
    }
    assert!(matches!(parse("#1"), Err(KeyError::BadDrawer { .. })));
    assert!(matches!(
        parse("not-a-uuid#1"),
        Err(KeyError::BadDrawer { .. })
    ));
}

#[test]
fn group_range_bounds_contain_only_this_drawers_suffixed_keys() {
    let (lo, hi) = group_range(ID);
    let inside = |k: &str| k >= lo.as_str() && k < hi.as_str();
    for suffix in ["#1", "#2", "#10", "#999999", "#0", "#x"] {
        let key = format!("{ID}{suffix}");
        assert!(inside(&key), "{key} belongs to the drawer's range");
    }
    assert!(!inside(ID), "the bare key is outside; callers add it");
    assert!(!inside(OTHER));
    assert!(!inside(&format!("{OTHER}#1")));
}
