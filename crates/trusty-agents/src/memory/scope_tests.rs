//! Unit tests for [`super`] — the per-assistant memory-palace key (#7443).
//!
//! Why: the key is the whole fix, so its two failure modes — an agent that
//! resolves to no palace, and a palace id that would escape the data root when
//! joined onto it — need direct coverage rather than only end-to-end coverage
//! through the tools.
//! What: construction validation, the #7428 resolution pointer, and the
//! scoped/unscoped palace-id formats.
//! Test: this module IS the test.

use std::path::PathBuf;

use super::{MemoryScope, MemoryScopeError, palace_id};
use crate::memory::store::Segment;

/// A tempdir standing in for `~/.trusty-agents/assistants`, with no agent dirs.
fn empty_roots() -> (tempfile::TempDir, Vec<PathBuf>) {
    let tmp = tempfile::tempdir().expect("tempdir");
    (tmp, Vec::new())
}

/// #7443: the scope is #7428's own palace, not a rule of this module's own.
#[test]
fn the_scope_is_the_7428_palace() {
    let (tmp, dirs) = empty_roots();
    let scope = MemoryScope::for_assistant_in(&dirs, tmp.path(), "izzie", None)
        .expect("a usable instance id resolves a palace");
    assert_eq!(
        scope.as_str(),
        "izzie",
        "the scope must be the instance-id palace #7428 derives"
    );
}

/// #7443 fail-closed: an unresolvable agent errors instead of falling through
/// to the shared palace.
#[test]
fn an_unresolvable_agent_name_is_an_error() {
    let (tmp, dirs) = empty_roots();
    // A name that is not a usable `AssistantInstanceId`, with no binding to
    // pin a palace: #7428 answers `PalaceSource::Unresolved`.
    let err = MemoryScope::for_assistant_in(&dirs, tmp.path(), "Not A Valid Instance Id!", None)
        .expect_err("an unresolvable agent must not get a scope");
    assert!(
        matches!(err, MemoryScopeError::Unresolved { .. }),
        "expected Unresolved, got {err:?}"
    );
}

/// #7443: a palace id that does not name a fresh child of the data root is
/// refused — `..` escapes the root, and a bare `.` IS the root, which puts
/// every assistant back in one shared directory.
#[test]
fn a_traversing_palace_id_is_rejected() {
    for bad in [".", "..", " . ", "./here", "../escape", "a/b", "a\\b", ""] {
        let err = MemoryScope::new(bad)
            .expect_err("a path-bearing or blank palace id must not become a scope");
        assert!(
            matches!(err, MemoryScopeError::Unusable { .. }),
            "expected Unusable for {bad:?}, got {err:?}"
        );
    }
    for ok in ["owner-profile.v2_1", "..foo", ".cache"] {
        assert!(
            MemoryScope::new(ok).is_ok(),
            "{ok:?} is a literal name, not a dot-segment, and stays usable"
        );
    }
}

/// #7443: two assistants key two palaces for the same segment.
#[test]
fn two_assistants_key_two_palaces() {
    let alpha = MemoryScope::new("alpha").expect("alpha");
    let beta = MemoryScope::new("beta").expect("beta");
    let a = palace_id(Some(&alpha), Segment::AgentMemory);
    let b = palace_id(Some(&beta), Segment::AgentMemory);
    assert_eq!(a, "trusty-agents-alpha-mem");
    assert_ne!(a, b, "two assistants must not share one AgentMemory palace");
}

/// #7443: the unscoped id keeps its pre-fix spelling, so the single-tenant
/// seeder and CLI stores read the palaces they already wrote.
#[test]
fn an_unscoped_palace_id_is_unchanged() {
    assert_eq!(
        palace_id(None, Segment::AgentMemory),
        "trusty-agents-mem",
        "an unscoped store must keep addressing its existing palace"
    );
    assert_eq!(palace_id(None, Segment::CodeIndex), "trusty-agents-code");
}
