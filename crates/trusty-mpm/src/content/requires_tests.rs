//! Tests for the advisory `requires:` block (#8390).

use super::*;

fn req(harness: &str, requirement: &str) -> Requirement {
    Requirement {
        harness: harness.to_owned(),
        requirement: requirement.to_owned(),
    }
}

/// #8390: both YAML forms read the same entries, in declaration order.
#[test]
fn requires_parses_flow_and_block_forms() {
    let flow = "---\nname: a\nrequires: {tm: \">=1.9.0\", tcode: \">=0.5\"}\n---\nbody\n";
    let block = "---\nname: a\nrequires:\n  tm: \">=1.9.0\"\n  tcode: \">=0.5\"\n---\nbody\n";
    let want = vec![req("tm", ">=1.9.0"), req("tcode", ">=0.5")];
    assert_eq!(parse_requires(flow), Ok(want.clone()));
    assert_eq!(parse_requires(block), Ok(want));
}

/// No frontmatter, no `requires:` key, or a `requires:` naming nothing all
/// declare no requirement — and an unrelated YAML error is not this block's.
#[test]
fn an_asset_without_requires_declares_none() {
    for text in [
        "# no frontmatter\n",
        "---\nname: a\ndescription: b\n---\nbody\n",
        "---\nname: a\nrequires:\n---\n",
        "---\ndescription: a: b\n---\n",
    ] {
        assert_eq!(parse_requires(text), Ok(Vec::new()), "{text}");
    }
}

/// #8390: a `requires:` the reader cannot use is an error naming why, never a
/// silently empty requirement list.
#[test]
fn a_malformed_requires_is_an_error() {
    assert_eq!(
        parse_requires("---\nrequires: \">=1.9.0\"\n---\n"),
        Err(RequiresError::Shape)
    );
    assert_eq!(
        parse_requires("---\nrequires: {tm: 1}\n---\n"),
        Err(RequiresError::Shape)
    );
    assert!(matches!(
        parse_requires("---\nrequires: {tm: \"one point nine\"}\n---\n"),
        Err(RequiresError::BadRequirement { ref harness, .. }) if harness == "tm"
    ));
    assert!(matches!(
        parse_requires("---\nrequires: {tm: \">=1\"\ndescription: a: b\n---\n"),
        Err(RequiresError::Frontmatter(_))
    ));
}

#[test]
fn requirement_matches_by_semver() {
    let v = semver::Version::new(1, 9, 0);
    assert!(req("tm", ">=1.9.0").satisfied_by(&v));
    assert!(req("tm", "^1.8").satisfied_by(&v));
    assert!(!req("tm", ">=999.0.0").satisfied_by(&v));
    assert!(!req("tm", "<1.9.0").satisfied_by(&v));
}
