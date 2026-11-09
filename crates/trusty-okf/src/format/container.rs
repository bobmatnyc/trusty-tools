//! OKF container rules (moved from trusty-kb `schema.rs`, #10746; SPEC-OKF-01
//! §2.7.2).
//!
//! Why: the container rules hold for every bundle, whatever profile it uses,
//! so they live in `format` and not in a profile.
//! What:
//!   - `type` is the ONLY required frontmatter field; every other field is
//!     optional and is never invented for a missing value ([`base_envelope`]).
//!   - `index.md` and `log.md` are RESERVED filenames, not concepts
//!     ([`RESERVED_FILES`]). OKF §3.1 reserves only these two.
//!   - Unknown frontmatter keys are preserved verbatim (merge, never strip) —
//!     enforced by [`super::concept::deep_merge`] and the raw-preserving
//!     render in [`super::raw`].
//!
//! Test: `type_is_only_required_field`, `reserved_files_are_not_entities`.

/// Reserved OKF filenames that are NOT entities and carry no frontmatter.
///
/// `index.md` is the generated per-folder README; `log.md` is the optional
/// append-only change log. Listing, validation, and conversion all skip these.
pub const RESERVED_FILES: [&str; 2] = ["index.md", "log.md"];

/// Whether a filename is an OKF reserved (non-entity) file.
///
/// Test: `reserved_files_are_not_entities`.
pub fn is_reserved_file(file_name: &str) -> bool {
    RESERVED_FILES.contains(&file_name)
}

/// The value-type a base-envelope field is expected to hold. Relationship
/// (edge) fields are validated structurally through the profile's inverse-edge
/// table, not listed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    /// A single scalar string (e.g. `title`, `uid`).
    Scalar,
    /// A YAML sequence of scalars (e.g. `aliases`, `same_as`, `sources`).
    List,
    /// An ISO-8601 date/time scalar (e.g. `created`, `updated`).
    Date,
}

/// One base-envelope field specification.
#[derive(Debug, Clone)]
pub struct FieldSpec {
    /// The frontmatter key.
    pub name: &'static str,
    /// The value shape expected for this key.
    pub ty: FieldType,
    /// When true, validation reports the entity if the key is absent. Under OKF
    /// this is set only for `type`.
    pub required: bool,
}

impl FieldSpec {
    const fn new(name: &'static str, ty: FieldType, required: bool) -> Self {
        Self { name, ty, required }
    }
}

/// The shared base-envelope field set (identical across collections).
///
/// Why: OKF's container is uniform — the collection only changes the default
/// `type` and which relationship verbs are idiomatic. Keeping one envelope list
/// avoids per-collection drift.
/// What: `type` required; everything else optional.
/// Test: `type_is_only_required_field`.
pub fn base_envelope() -> Vec<FieldSpec> {
    vec![
        FieldSpec::new("type", FieldType::Scalar, true),
        FieldSpec::new("title", FieldType::Scalar, false),
        FieldSpec::new("aliases", FieldType::List, false),
        FieldSpec::new("description", FieldType::Scalar, false),
        FieldSpec::new("tags", FieldType::List, false),
        FieldSpec::new("uid", FieldType::Scalar, false),
        FieldSpec::new("same_as", FieldType::List, false),
        FieldSpec::new("sources", FieldType::List, false),
        FieldSpec::new("created", FieldType::Date, false),
        FieldSpec::new("updated", FieldType::Date, false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: OKF's defining rule is that `type` is the ONLY required field.
    /// What: asserts exactly one envelope field is required, and it is `type`.
    /// Test: self-contained.
    #[test]
    fn type_is_only_required_field() {
        let required: Vec<&str> = base_envelope()
            .into_iter()
            .filter(|f| f.required)
            .map(|f| f.name)
            .collect();
        assert_eq!(required, vec!["type"]);
    }

    /// Why: reserved files must never be treated as entities.
    /// What: asserts index.md/log.md are reserved and a normal file is not.
    /// Test: self-contained.
    #[test]
    fn reserved_files_are_not_entities() {
        assert!(is_reserved_file("index.md"));
        assert!(is_reserved_file("log.md"));
        assert!(!is_reserved_file("ada-lovelace.md"));
    }
}
