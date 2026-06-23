//! Session ID type for the SESSCTL control plane.
//!
//! Why: session IDs must be stable, unambiguous, and human-readable across
//! every surface (CLI, HTTP, TUI, TELUI). A dedicated newtype enforces the
//! `<project-id>-<N>` convention and provides typed parse / display without
//! accidentally mixing in the UUID-based `ManagedSessionId` from the old path.
//! What: [`ControlSessionId`] is a newtype over `String` following the
//! `<project-id>-<sessionNo>` pattern (§5.1 of SPEC-SESSCTL-01). The
//! companion [`SessionCounter`] allocates monotonically-increasing session
//! numbers per project within a daemon lifetime.
//! Test: `control_session_id_display`, `control_session_id_parse`,
//! `session_counter_monotonic` in the inline test module.

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// A stable session identifier following the `<project-id>-<sessionNo>` convention.
///
/// Why: mixing UUID-based identifiers (used by the old managed-session path)
/// with the new slug-based IDs would create ambiguity at every API surface.
/// A dedicated newtype makes session-ID kind errors a compile-time catch.
/// What: wraps a `String` of the form `<project-id>-<N>` where N is a
/// non-negative integer scoped per project per daemon lifetime.
/// Test: `control_session_id_display`, `control_session_id_parse`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ControlSessionId(pub String);

impl ControlSessionId {
    /// Construct a session ID from a project name and monotonic session number.
    ///
    /// Why: the allocation point in `SessionCounter::next` is the only place
    /// where raw strings should be assembled; callers use typed IDs.
    /// What: formats `<project_id>-<n>` as the canonical ID.
    /// Test: `control_session_id_display`.
    pub fn new(project_id: &str, n: u64) -> Self {
        Self(format!("{project_id}-{n}"))
    }

    /// Return the string representation (same as `Display`).
    ///
    /// Why: handler code needs a `&str` reference without cloning.
    /// What: returns a reference to the inner string.
    /// Test: `control_session_id_display`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse a session ID into its project-id and session-number components.
    ///
    /// Why: list and routing code sometimes needs to extract the project scope
    /// from an opaque session ID without a registry lookup.
    /// What: splits on the last `-` and parses the suffix as `u64`; returns
    /// `None` if the format does not match.
    /// Test: `control_session_id_parse`.
    pub fn parse_parts(&self) -> Option<(&str, u64)> {
        let s = self.0.as_str();
        let dash = s.rfind('-')?;
        let suffix = &s[dash + 1..];
        let n: u64 = suffix.parse().ok()?;
        Some((&s[..dash], n))
    }
}

impl fmt::Display for ControlSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Per-project monotonic session-number allocator.
///
/// Why: session numbers must never be reused within a daemon lifetime so that
/// session IDs remain globally unambiguous even after a session is stopped.
/// What: maintains a `HashMap<project_id, next_n>` and increments atomically
/// on each `next()` call. Not shared across threads; callers place it behind
/// the `SessionRegistry`'s `RwLock`.
/// Test: `session_counter_monotonic`.
#[derive(Debug, Default)]
pub struct SessionCounter {
    counters: HashMap<String, u64>,
}

impl SessionCounter {
    /// Allocate the next `ControlSessionId` for the given project.
    ///
    /// Why: every `tm run <project-id>` must produce a unique, never-recycled
    /// session ID for that project within this daemon lifetime.
    /// What: looks up (or inserts) the counter for `project_id`, returns a
    /// `ControlSessionId` with the current value, then increments.
    /// Test: `session_counter_monotonic`.
    pub fn next(&mut self, project_id: &str) -> ControlSessionId {
        let n = self.counters.entry(project_id.to_owned()).or_insert(0);
        let id = ControlSessionId::new(project_id, *n);
        *n += 1;
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_session_id_display() {
        let id = ControlSessionId::new("trusty-tools", 3);
        assert_eq!(id.to_string(), "trusty-tools-3");
        assert_eq!(id.as_str(), "trusty-tools-3");
    }

    #[test]
    fn control_session_id_parse() {
        let id = ControlSessionId::new("my-proj", 0);
        let (proj, n) = id.parse_parts().expect("should parse");
        assert_eq!(proj, "my-proj");
        assert_eq!(n, 0);
    }

    #[test]
    fn control_session_id_parse_project_with_dashes() {
        let id = ControlSessionId("trusty-tools-2".to_owned());
        let (proj, n) = id.parse_parts().expect("should parse");
        assert_eq!(proj, "trusty-tools");
        assert_eq!(n, 2);
    }

    #[test]
    fn control_session_id_parse_invalid() {
        let id = ControlSessionId("no-number".to_owned());
        assert!(id.parse_parts().is_none());
    }

    #[test]
    fn session_counter_monotonic() {
        let mut counter = SessionCounter::default();
        let a = counter.next("proj");
        let b = counter.next("proj");
        let c = counter.next("other");
        assert_eq!(a.as_str(), "proj-0");
        assert_eq!(b.as_str(), "proj-1");
        assert_eq!(c.as_str(), "other-0");
        // proj counter is at 2
        let d = counter.next("proj");
        assert_eq!(d.as_str(), "proj-2");
    }
}
