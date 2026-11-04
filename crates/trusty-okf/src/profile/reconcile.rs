//! Inverse-edge reconciler — materialise the other side of every relationship
//! (moved from trusty-kb `reconcile.rs`, #10746).
//!
//! Why: the profile declares relationship edges in pairs (parent_of↔child_of,
//! works_at↔employee, …) and symmetric verbs (knows, spouse_of, …). Writing one
//! side should deterministically create the mirror on the target so the link
//! graph is navigable from either endpoint. The reconciler is idempotent: a
//! second run finds the inverse already present and does nothing.
//!
//! What: the pure half of reconciliation. [`source_link`] names the link text
//! targets use to point back at a source; [`inverse_links`] walks a source
//! entity's edge fields (any key with an inverse in the profile's edge table)
//! and returns each `(target, inverse field)` pair; [`add_inverse_link`] unions
//! a `[[source]]` link into one target's inverse field. Resolving a target to
//! a file and writing it is the caller's file walk (`Bundle` from slice S3;
//! trusty-kb's `KbStore::reconcile_entity` until then). Dangling links are
//! left for validation to report; targets are never fabricated.
//!
//! Test: `inverse_links_follow_profile_and_skip_self`,
//! `add_inverse_link_unions_once_and_touches_updated`.

use serde_yaml::Value;

use crate::format::concept::{Entity, link_values, slugify};
use crate::profile::Profile;

/// The link text a source's targets use to point back at it: its `title`,
/// else its name.
pub fn source_link(source_name: &str, source: &Entity) -> String {
    source
        .get_str("title")
        .map(str::to_string)
        .unwrap_or_else(|| source_name.to_string())
}

/// The `(target, inverse field)` pairs to materialise for one source entity.
///
/// What: for every frontmatter key with an inverse in `profile`, every
/// `[[target]]` in its value yields `(target, inverse)`. A target whose slug
/// equals the source link's slug is skipped (never a self-loop record on the
/// same file).
/// Test: `inverse_links_follow_profile_and_skip_self`.
pub fn inverse_links(
    profile: &Profile,
    source_link: &str,
    source: &Entity,
) -> Vec<(String, &'static str)> {
    let Value::Mapping(map) = &source.frontmatter else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (k, v) in map {
        let Some(field) = k.as_str() else { continue };
        let Some(inverse) = profile.inverse_of(field) else {
            continue;
        };
        for target in link_values(v) {
            if slugify(&target) == slugify(source_link) {
                continue;
            }
            out.push((target, inverse));
        }
    }
    out
}

/// Union a `[[source_link]]` link into the `field` edge of `target`.
///
/// What: returns false and leaves `target` unchanged when the field already
/// links the source (by slug). Otherwise normalises the field to a sorted,
/// de-duplicated sequence of link scalars with the new link added, sets
/// `updated` to `now` (the target's content changed), and returns true.
/// Test: `add_inverse_link_unions_once_and_touches_updated`.
pub fn add_inverse_link(target: &mut Entity, field: &str, source_link: &str, now: &str) -> bool {
    let link = format!("[[{source_link}]]");
    let want_slug = slugify(source_link);

    let map = target.map_mut();
    let key = Value::String(field.to_string());
    let already = map
        .get(&key)
        .map(|v| link_values(v).iter().any(|t| slugify(t) == want_slug))
        .unwrap_or(false);
    if already {
        return false;
    }
    // Normalise the field to a sorted sequence of link scalars.
    let mut links: Vec<String> = map
        .get(&key)
        .map(|v| link_values(v).iter().map(|t| format!("[[{t}]]")).collect())
        .unwrap_or_default();
    links.push(link);
    links.sort();
    links.dedup();
    map.insert(
        key,
        Value::Sequence(links.into_iter().map(Value::String).collect()),
    );
    // Touch `updated` since the target's content changed.
    map.insert(
        Value::String("updated".into()),
        Value::String(now.to_string()),
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(y: &str) -> Entity {
        Entity::from_content(&format!("---\n{y}---\n")).unwrap()
    }

    /// Why: symmetric verbs mirror the same field, pairs mirror the paired
    /// verb, and a self-link must not loop back.
    /// What: Ada knows Grace, is parent_of Leon and knows herself; asserts the
    /// two pairs and no self pair.
    /// Test: self-contained.
    #[test]
    fn inverse_links_follow_profile_and_skip_self() {
        let p = Profile::default_profile();
        let ada =
            entity("title: Ada\nknows: [\"[[Grace]]\", \"[[Ada]]\"]\nparent_of: \"[[Leon]]\"\n");
        let mut got = inverse_links(&p, &source_link("ada", &ada), &ada);
        got.sort();
        assert_eq!(
            got,
            vec![
                ("Grace".to_string(), "knows"),
                ("Leon".to_string(), "child_of")
            ]
        );
    }

    /// Why: re-running the reconciler must not churn the target.
    /// What: adds the same inverse twice; the second call returns false and
    /// leaves the entity equal.
    /// Test: self-contained.
    #[test]
    fn add_inverse_link_unions_once_and_touches_updated() {
        let mut grace = entity("title: Grace\nknows: \"[[Babbage]]\"\n");
        assert!(add_inverse_link(
            &mut grace,
            "knows",
            "Ada",
            "2026-01-01T00:00:00Z"
        ));
        let links = link_values(grace.frontmatter.get("knows").unwrap());
        assert_eq!(links, vec!["Ada".to_string(), "Babbage".to_string()]);
        assert_eq!(grace.get_str("updated"), Some("2026-01-01T00:00:00Z"));
        let before = grace.clone();
        assert!(!add_inverse_link(
            &mut grace,
            "knows",
            "Ada",
            "2026-02-02T00:00:00Z"
        ));
        assert_eq!(grace, before);
    }
}
