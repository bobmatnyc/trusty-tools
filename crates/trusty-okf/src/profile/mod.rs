//! Collection profiles and inverse-edge reconciliation (moved from trusty-kb
//! `schema.rs`, #10746; SPEC-OKF-01 §2.7.2).
//!
//! Why: a bundle's collection layout and relationship verbs are pluggable, so a
//! researched schema can replace one profile without touching store, convert or
//! validate logic. The OKF container rules hold for every profile and live in
//! [`crate::format::container`]. A profile names no agent.
//!
//! What: [`Profile`] lists the collections and carries its inverse-edge table.
//! [`Profile::default_profile`] is the personal-KB profile trusty-kb shipped:
//! the OKF container with a snake_case schema.org/FOAF relationship profile
//! layered on top. It ships as one profile, not as the container default.
//!   - Folder-per-type collections, each mapped to a schema.org type where one
//!     exists (see [`CollectionSpec::schema_type`]).
//!   - A shared base envelope ([`crate::format::container::base_envelope`]):
//!     `type` (required), `title`, `aliases[]`, `description`, `tags[]`, `uid`
//!     (stable rename-safe id), `same_as[]` (external authority URIs — the
//!     entity-resolution/dedup key), `sources[]` (provenance, distinct from
//!     `same_as`), `created`/`updated`.
//!   - snake_case relationship fields holding `[[wiki-links]]` (quoted scalar or
//!     list), per collection (see [`CollectionSpec::edge_fields`]).
//!
//! Inverse-edge reconciler ([`EDGE_PAIRS`] / [`Profile::inverse_of`]): writing
//! one side of a relation materialises the other deterministically (see
//! [`reconcile`]). Bidirectional pairs: parent_of↔child_of,
//! works_at↔employee, member_of↔member, parent_org↔sub_org,
//! located_in↔contains, mentor_of↔apprentice_to. Symmetric verbs (mirror the
//! same verb on the target): knows, sibling_of, spouse_of, colleague_of,
//! family_of, neighbor_of.
//!
//! schema.org / FOAF export mapping (DOCUMENTED, not built — a future JSON-LD
//! exporter would consume it): title→schema:name/foaf:name, aliases→schema:
//! alternateName, description→schema:description, same_as→schema:sameAs,
//! knows→foaf:knows, works_at→schema:worksFor, member_of→schema:memberOf,
//! located_in→schema:containedInPlace (inverse schema:containsPlace),
//! parent_of→schema:parent (inverse schema:children), spouse_of→schema:spouse,
//! founder→schema:founder, organizer→schema:organizer, attendee→schema:
//! attendee, about→schema:about, creator→schema:creator, owner→schema:owner.
//! JSON-LD frontmatter, PARA/Johnny-Decimal, Dendron schema.yml, Dataview
//! inline fields, RDF serialisation, and the LogSeq block model are explicitly
//! NOT adopted.
//!
//! Test: `default_profile_has_core_collections`,
//! `edge_inverses_are_symmetric`, `profile_carries_its_edge_table`.

pub mod reconcile;

/// One collection: a top-level directory holding entities of a kind.
#[derive(Debug, Clone)]
pub struct CollectionSpec {
    /// Directory name under the KB root (e.g. `people`).
    pub name: &'static str,
    /// The schema.org type new entities default their `type` to, if any.
    /// `None` for `projects` (OKF uses a free `type` string there).
    pub schema_type: Option<&'static str>,
    /// One-line purpose, surfaced in the generated `index.md`.
    pub purpose: &'static str,
    /// snake_case relationship field names valid for this collection (for docs
    /// and README generation). Reconciliation itself is driven by the
    /// profile's edge table ([`Profile::edge_pairs`]), not this list.
    pub edge_fields: &'static [&'static str],
}

/// Bidirectional + symmetric edge pairs `(forward, inverse)`.
///
/// The personal-KB profile's edge table. Symmetric verbs appear as
/// `(verb, verb)`. The inverse relation is materialised on the link target by
/// [`reconcile`]. A profile's table is the single source of truth for "which
/// frontmatter keys are relationship edges".
pub const EDGE_PAIRS: &[(&str, &str)] = &[
    // Bidirectional
    ("parent_of", "child_of"),
    ("child_of", "parent_of"),
    ("works_at", "employee"),
    ("employee", "works_at"),
    ("member_of", "member"),
    ("member", "member_of"),
    ("parent_org", "sub_org"),
    ("sub_org", "parent_org"),
    ("located_in", "contains"),
    ("contains", "located_in"),
    ("mentor_of", "apprentice_to"),
    ("apprentice_to", "mentor_of"),
    // Symmetric (mirror the same verb on the target)
    ("knows", "knows"),
    ("sibling_of", "sibling_of"),
    ("spouse_of", "spouse_of"),
    ("colleague_of", "colleague_of"),
    ("family_of", "family_of"),
    ("neighbor_of", "neighbor_of"),
];

/// The inverse relationship name for an edge field, if it is a known edge.
///
/// Why: callers that predate pluggable profiles (a graph read with no profile)
/// ask "is this key a relationship, and what mirrors it?" without a profile.
/// What: `Some(inverse)` for any key in [`EDGE_PAIRS`] (a symmetric verb maps to
/// itself), `None` for a non-edge key. Profile-aware code calls
/// [`Profile::inverse_of`].
/// Test: `edge_inverses_are_symmetric`.
pub fn inverse_edge(field: &str) -> Option<&'static str> {
    inverse_in(EDGE_PAIRS, field)
}

fn inverse_in(pairs: &[(&'static str, &'static str)], field: &str) -> Option<&'static str> {
    pairs.iter().find(|(f, _)| *f == field).map(|(_, inv)| *inv)
}

/// The active schema profile: the set of known collections and the edge table.
#[derive(Debug, Clone)]
pub struct Profile {
    /// Known collections, in the deterministic order they are created/rendered.
    pub collections: Vec<CollectionSpec>,
    /// #10746: the `(forward, inverse)` edge pairs reconciliation uses
    /// (SPEC-OKF-01 §2.7.2, "inverse edges come from the profile").
    pub edge_pairs: &'static [(&'static str, &'static str)],
}

impl Profile {
    /// The personal-KB profile: OKF container + schema.org/FOAF relations.
    ///
    /// Why: the shipped baseline; a different research outcome replaces this fn.
    /// What: the six folder-per-type collections with their schema.org types and
    /// idiomatic relationship verbs, and [`EDGE_PAIRS`] as the edge table.
    /// Test: `default_profile_has_core_collections`.
    pub fn default_profile() -> Self {
        const PERSON: &[&str] = &[
            "knows",
            "works_at",
            "member_of",
            "colleague_of",
            "spouse_of",
            "parent_of",
            "child_of",
            "sibling_of",
            "family_of",
            "mentor_of",
            "located_in",
        ];
        const ORG: &[&str] = &[
            "member",
            "employee",
            "founder",
            "parent_org",
            "sub_org",
            "located_in",
        ];
        const PLACE: &[&str] = &["located_in", "contains"];
        const EVENT: &[&str] = &["location", "organizer", "attendee", "about"];
        const PROJECT: &[&str] = &["member", "sponsor", "located_in", "related_to"];
        const THING: &[&str] = &["creator", "owner", "located_in", "about"];
        Self {
            collections: vec![
                CollectionSpec {
                    name: "people",
                    schema_type: Some("Person"),
                    purpose: "Individual people the assistant tracks.",
                    edge_fields: PERSON,
                },
                CollectionSpec {
                    name: "organizations",
                    schema_type: Some("Organization"),
                    purpose: "Companies, teams, and institutions.",
                    edge_fields: ORG,
                },
                CollectionSpec {
                    name: "places",
                    schema_type: Some("Place"),
                    purpose: "Locations, venues, and geographic entities.",
                    edge_fields: PLACE,
                },
                CollectionSpec {
                    name: "events",
                    schema_type: Some("Event"),
                    purpose: "Meetings, milestones, and dated occurrences.",
                    edge_fields: EVENT,
                },
                CollectionSpec {
                    name: "projects",
                    schema_type: None,
                    purpose: "Ongoing efforts and initiatives.",
                    edge_fields: PROJECT,
                },
                CollectionSpec {
                    name: "things",
                    schema_type: Some("Thing"),
                    purpose: "Objects, products, concepts, and everything else.",
                    edge_fields: THING,
                },
            ],
            edge_pairs: EDGE_PAIRS,
        }
    }

    /// The inverse of an edge field under this profile's edge table.
    ///
    /// Test: `profile_carries_its_edge_table`.
    pub fn inverse_of(&self, field: &str) -> Option<&'static str> {
        inverse_in(self.edge_pairs, field)
    }

    /// Look up a collection spec by directory name.
    ///
    /// Test: `default_profile_has_core_collections`.
    pub fn collection(&self, name: &str) -> Option<&CollectionSpec> {
        self.collections.iter().find(|c| c.name == name)
    }

    /// Whether `name` is one of the profile's known collections.
    ///
    /// Test: `default_profile_has_core_collections`.
    pub fn is_known_collection(&self, name: &str) -> bool {
        self.collection(name).is_some()
    }

    /// The known collection directory names, in profile order.
    pub fn collection_names(&self) -> Vec<&'static str> {
        self.collections.iter().map(|c| c.name).collect()
    }

    /// The default `type` value for a new entity created in `collection`.
    ///
    /// Why: OKF requires `type`; when a caller omits it, the collection's
    /// schema.org type is the honest default (never a guessed value).
    /// What: the collection's `schema_type` if known, else `Note` (the
    /// container-level fail-open type for a free/unknown collection).
    /// Test: `default_type_for_collection_falls_back_to_note`.
    pub fn default_type_for(&self, collection: &str) -> String {
        self.collection(collection)
            .and_then(|c| c.schema_type)
            .unwrap_or("Note")
            .to_string()
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self::default_profile()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the six folder-per-type collections are the baseline contract.
    /// What: asserts the collection set, order, and a schema.org type mapping.
    /// Test: self-contained.
    #[test]
    fn default_profile_has_core_collections() {
        let p = Profile::default_profile();
        assert_eq!(
            p.collection_names(),
            vec![
                "people",
                "organizations",
                "places",
                "events",
                "projects",
                "things"
            ]
        );
        assert_eq!(p.collection("people").unwrap().schema_type, Some("Person"));
        assert_eq!(p.collection("projects").unwrap().schema_type, None);
        assert!(p.is_known_collection("things"));
        assert!(!p.is_known_collection("recipes"));
    }

    /// Why: the reconciler depends on the inverse map being an involution —
    /// inverse(inverse(x)) == x — or edges would not reconcile on re-run.
    /// What: asserts every forward edge's inverse maps back to it.
    /// Test: self-contained.
    #[test]
    fn edge_inverses_are_symmetric() {
        for (forward, inverse) in EDGE_PAIRS {
            let back = inverse_edge(inverse).expect("inverse must itself be an edge");
            assert_eq!(back, *forward, "inverse of {inverse} should be {forward}");
        }
        assert_eq!(inverse_edge("knows"), Some("knows"));
        assert_eq!(inverse_edge("parent_of"), Some("child_of"));
        assert_eq!(inverse_edge("not_an_edge"), None);
    }

    /// Why: reconciliation reads inverses from the profile, so a profile with
    /// another table must change the answer.
    /// What: the default profile agrees with [`EDGE_PAIRS`]; a profile with an
    /// empty table knows no edge.
    /// Test: self-contained.
    #[test]
    fn profile_carries_its_edge_table() {
        let p = Profile::default_profile();
        assert_eq!(p.inverse_of("works_at"), Some("employee"));
        let bare = Profile {
            edge_pairs: &[],
            ..Profile::default_profile()
        };
        assert_eq!(bare.inverse_of("works_at"), None);
    }

    /// Why: OKF fail-open — an unknown collection still yields a valid `type`.
    /// What: asserts `Note` fallback for a free collection.
    /// Test: self-contained.
    #[test]
    fn default_type_for_collection_falls_back_to_note() {
        let p = Profile::default_profile();
        assert_eq!(p.default_type_for("people"), "Person");
        assert_eq!(p.default_type_for("recipes"), "Note");
    }
}
