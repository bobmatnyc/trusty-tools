//! The one trusty-search index id derived from a tm registry project (#10530).
//!
//! Why: three derivations of an index id coexisted. `derive_index_id` is the
//! root basename (what registration creates), `ProjectIdentity::index_id` is
//! `<owner-repo>-<16 hex>` over origin, root and operator (what tm published in
//! the active set), and operators hand-name. The published id matched no
//! registered index, and both derived forms changed on `mv`, a repo rename or
//! `git remote remove`. See the index-hygiene design brief, section 1.
//! What: [`RegisteredProject`] is the `(name, project_key)` pair tm stores once
//! at register. [`derive_registry_index_id`] turns it into
//! `<slug(name)>-<8 hex of sha256("v2:" + project_key)>`. [`index_id_for_root`]
//! is the single choice every caller routes through. It returns the legacy
//! basename until [`REGISTRY_IDS_ADOPTED`] flips (D7), because trusty-code cannot
//! yet look up a registry key and the two must name one index. Pure; no I/O.
//! Test: `registry_index_id_tests.rs`.

use std::path::Path;

/// Scheme tag mixed into the digest preimage so a later scheme is disjoint.
const SCHEME_TAG: &str = "v2:";

/// Hex digits of digest in the id. Matches the existing suffixed ids.
const DIGEST_HEX: usize = 8;

/// Longest label, in characters, before the digest.
const MAX_LABEL_LEN: usize = 40;

/// Label used when the project name slugifies to nothing.
const FALLBACK_LABEL: &str = "project";

/// A project as the tm registry records it: a name and a key stored once.
///
/// Why: `trusty-common` cannot read tm's registry, so tm hands the pair across.
/// What: `project_key` is `owner/repo` or `local:<uuid>`, assigned at register
/// and never recomputed; `name` is the registry key (cosmetic label only).
/// Test: `registry_index_id_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredProject {
    /// Registry name; only the label of the id derives from it.
    pub name: String,
    /// Stable project key; the digest of the id derives from it.
    pub project_key: String,
}

impl RegisteredProject {
    /// Build a registered-project identity.
    ///
    /// Why: one constructor keeps call sites free of struct-literal churn.
    /// Test: `registry_index_id_tests.rs`.
    pub fn new(name: impl Into<String>, project_key: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            project_key: project_key.into(),
        }
    }
}

/// Derive `<slug(name)>-<8 hex of sha256("v2:" + project_key)>`.
///
/// Why: the id must survive `mv`, a repo rename and `git remote remove`, so it
/// takes only values tm stores once. The label is cosmetic and the digest
/// carries uniqueness, so two projects sharing a name still differ.
/// What: slugifies `name` (truncated to 40 chars, `project` when empty) and
/// appends the first 8 hex digits of the SHA-256 of `"v2:" + project_key`.
///
/// # Postconditions
/// Non-empty; one URL path segment matching `[a-z0-9][a-z0-9-]*`; a pure
/// function of its two arguments.
/// Test: `derivation_is_pinned_for_a_known_input`,
/// `same_key_gives_same_id_whatever_the_name_spelling_of_the_path`,
/// `different_keys_give_different_ids`.
#[must_use]
pub fn derive_registry_index_id(name: &str, project_key: &str) -> String {
    use sha2::{Digest, Sha256};
    debug_assert!(!project_key.is_empty(), "project_key must be non-empty");
    let mut label: String = crate::slug::slugify_string(name)
        .chars()
        .take(MAX_LABEL_LEN)
        .collect();
    while label.ends_with('-') {
        label.pop();
    }
    if label.is_empty() {
        label = FALLBACK_LABEL.to_string();
    }
    let digest = Sha256::digest(format!("{SCHEME_TAG}{project_key}").as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("{label}-{}", &hex[..DIGEST_HEX])
}

/// Whether index NAMING has switched to the registry-derived id (#10530).
///
/// Why: tm and trusty-code share one daemon index per root. trusty-code has no
/// registry access, so if tm alone named indexes by registry id, each tm project
/// would get two indexes (a pile-up D1 itself would cause). Both sides switch in
/// the same change (D7); until then every caller keeps the legacy basename.
/// What: `false`. Flip it only in the change that also gives trusty-code the
/// registry lookup, and update the tests that pin the legacy behaviour.
/// Test: `index_id_for_root_stays_legacy_until_adoption`.
pub const REGISTRY_IDS_ADOPTED: bool = false;

/// The registry-derived id for `root` when `registered` owns it, else the
/// legacy basename. Ignores [`REGISTRY_IDS_ADOPTED`].
///
/// Why: the adopted behaviour must be testable, and callers that must accept
/// either form (the doctor) need the registry id by name.
/// Test: `registry_id_for_root_prefers_the_registered_identity`,
/// `registry_id_for_root_falls_back_to_the_legacy_basename`.
#[must_use]
pub fn registry_id_for_root(root: &Path, registered: Option<&RegisteredProject>) -> String {
    match registered {
        Some(p) => derive_registry_index_id(&p.name, &p.project_key),
        None => crate::derive_index_id(root),
    }
}

/// The index id every caller names `root`'s index by.
///
/// Why: registration, tm's published active set and the doctor probe must name
/// one index per project, and trusty-code must name the same one. Routing all
/// of them through this function makes that true by construction.
/// What: the legacy [`crate::derive_index_id`] basename while
/// [`REGISTRY_IDS_ADOPTED`] is `false`; [`registry_id_for_root`] once it is
/// `true`. `root` must already be the resolved project root
/// ([`crate::resolve_project_root`]).
/// Test: `index_id_for_root_stays_legacy_until_adoption`.
#[must_use]
pub fn index_id_for_root(root: &Path, registered: Option<&RegisteredProject>) -> String {
    if REGISTRY_IDS_ADOPTED {
        registry_id_for_root(root, registered)
    } else {
        crate::derive_index_id(root)
    }
}

#[cfg(test)]
#[path = "registry_index_id_tests.rs"]
mod registry_index_id_tests;
