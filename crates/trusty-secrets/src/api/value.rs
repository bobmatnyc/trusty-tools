//! [`SecretValue`]: the only type in this crate that holds plaintext.
//!
//! Why: a value must be able to cross the wire (`secrets.set`) and reach a
//! backend, but must never reach a log line. Keeping it in one newtype with a
//! redacting `Debug` and no `Display` means any struct that derives `Debug`
//! around it stays safe.
//! What: a `String` wrapper, serde-transparent, with `expose` as the single
//! deliberate read path.
//! Test: `api_debug_of_value_carrying_types_hides_the_value`.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A secret value.
///
/// Why: see the module docs.
/// What: `Debug` renders `SecretValue(<redacted, N chars>)`. There is no
/// `Display`, no `PartialEq`, and no `Deref`; read the plaintext with
/// [`SecretValue::expose`].
/// Test: `api_debug_of_value_carrying_types_hides_the_value`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretValue(String);

impl SecretValue {
    /// Wrap a plaintext value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The plaintext. Every call site is a deliberate disclosure point.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Length in characters, the unit `list` and `mask_secret` report.
    pub fn char_len(&self) -> usize {
        self.0.chars().count()
    }

    /// Whether the value is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretValue(<redacted, {} chars>)", self.char_len())
    }
}
