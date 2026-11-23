//! The new `[[stores]] root` field and its confinement (#4325).
//!
//! Why: this is #4325 requirement 1 — "per-assistant root field on
//! AgentStoreBinding", the data-model gap #3890 recorded. The field only means
//! "this instance's own store" if it cannot name someone else's, so the reject
//! cases matter as much as the accept ones.
//! What: the derived default, an explicit relative root, and every escape a
//! hand-edited `agent.toml` could attempt.
//! Test: this module IS the test surface.

use super::home_tests::temp_home;
use crate::assistants::{AssistantError, OKG_DIR};
use crate::stores::AgentStoreBinding;

fn binding(root: Option<&str>) -> AgentStoreBinding {
    AgentStoreBinding {
        name: "izzie-kb".to_string(),
        root: root.map(str::to_string),
        ..Default::default()
    }
}

/// Why: the default is what makes "each instance carries its own OKG store"
/// true without every `agent.toml` having to say so.
#[test]
fn omitted_root_defaults_to_the_home_okg_dir() {
    let (_tmp, home) = temp_home();
    assert_eq!(home.store_root(&binding(None)).unwrap(), home.okg_dir());
    // A blank/whitespace root is the same as omitting it, not an error.
    assert_eq!(
        home.store_root(&binding(Some("  "))).unwrap(),
        home.okg_dir()
    );
}

#[test]
fn declared_relative_root_resolves_inside_the_home() {
    let (_tmp, home) = temp_home();
    assert_eq!(
        home.store_root(&binding(Some("okg/personal"))).unwrap(),
        home.path().join(OKG_DIR).join("personal")
    );
    assert_eq!(
        home.store_root(&binding(Some("./knowledge"))).unwrap(),
        home.path().join("knowledge")
    );
}

/// Why: an instance's store must not be able to name another instance's — the
/// same silent-wrong-target hazard `stores::binding` refuses to guess at.
#[test]
fn rejects_a_root_that_climbs_out_of_the_home() {
    let (_tmp, home) = temp_home();
    let err = home
        .store_root(&binding(Some("../cto-assistant/okg")))
        .expect_err("must reject");
    assert!(
        matches!(err, AssistantError::UnconfinedStoreRoot { .. }),
        "wrong error: {err}"
    );
    assert!(err.to_string().contains("climbs out"), "was: {err}");
}

#[test]
fn rejects_an_absolute_root() {
    let (_tmp, home) = temp_home();
    let err = home
        .store_root(&binding(Some("/tmp/okg")))
        .expect_err("must reject");
    assert!(err.to_string().contains("absolute path"), "was: {err}");
}

#[test]
fn rejects_a_root_that_is_the_home_itself() {
    let (_tmp, home) = temp_home();
    let err = home
        .store_root(&binding(Some(".")))
        .expect_err("must reject");
    assert!(
        err.to_string().contains("home directory itself"),
        "was: {err}"
    );
}

/// Why: the field is new, so an existing `agent.toml` with no `root` must
/// parse unchanged and validate exactly as before.
#[test]
fn root_is_optional_and_validated_in_the_binding() {
    let existing: AgentStoreBinding =
        toml::from_str("name = \"bob-kb\"\ntree = \"okg://izzie\"\nindex = \"bob-kb\"").unwrap();
    assert_eq!(existing.root, None);
    assert_eq!(existing.validate(), None);

    let with_root: AgentStoreBinding = toml::from_str("name = \"bob-kb\"\nroot = \"okg\"").unwrap();
    assert_eq!(with_root.root.as_deref(), Some("okg"));
    assert_eq!(with_root.validate(), None);

    let escaping: AgentStoreBinding =
        toml::from_str("name = \"bob-kb\"\nroot = \"../elsewhere\"").unwrap();
    let reason = escaping.validate().expect("must be flagged");
    assert!(
        reason.contains("relative to the assistant"),
        "was: {reason}"
    );
}
