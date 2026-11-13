//! Project identity, the key of every bundle, event and store partition.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{DgError, Result};

const PROJECT_ID_MAX: usize = 128;

/// A validated project id: 1..=128 chars of `[a-z0-9._-]`.
///
/// Why: per-project isolation (§3) compares ids, and the store (PR3) uses the
/// id as a directory name, so it must be a safe path segment.
/// What: a `String` newtype; `.` and `..` are refused as path traversal.
/// Test: `project_id_rejects_unsafe_values`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectId(String);

impl ProjectId {
    /// Validates and wraps `id`.
    pub fn new(id: impl Into<String>) -> Result<Self> {
        Self::try_from(id.into())
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Refuses `other` unless it is this project.
    ///
    /// Test: `bundle_for_a_from_b_input_is_refused`.
    pub fn require_same(&self, other: &ProjectId) -> Result<()> {
        if self == other {
            Ok(())
        } else {
            Err(DgError::ForeignProject {
                expected: self.0.clone(),
                found: other.0.clone(),
            })
        }
    }
}

impl TryFrom<String> for ProjectId {
    type Error = DgError;

    fn try_from(value: String) -> Result<Self> {
        let charset_ok = value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
        let ok = !value.is_empty()
            && value.len() <= PROJECT_ID_MAX
            && charset_ok
            && value != "."
            && value != "..";
        if ok {
            Ok(Self(value))
        } else {
            Err(DgError::InvalidProjectId)
        }
    }
}

impl From<ProjectId> for String {
    fn from(value: ProjectId) -> Self {
        value.0
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
