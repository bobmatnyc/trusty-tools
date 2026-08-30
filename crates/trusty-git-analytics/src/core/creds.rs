//! Credential lookup as a parameter instead of a process-global read.
//!
//! Why (#6405): every source client used to read its token straight from
//! `std::env::var`, so the only way to prove a token-dependent path was to
//! `set_var` it. `setenv` can reallocate the environment array while another
//! thread is inside `getenv`, and `cargo test -p tga` runs dozens of threads
//! that call `getenv` through reqwest, rustls and tracing — the family behind
//! the #2613 SIGSEGV. Passing the lookup in removes the mutation entirely.
//!
//! What: [`CredentialSource`] wraps a name → value function.
//! [`CredentialSource::from_env`] is the production one; the test constructors
//! answer from a fixed table.
//!
//! Test: `crate::core::creds::tests` here, plus every caller listed on
//! [`CredentialSource::get`].

use std::sync::Arc;

/// The lookup a [`CredentialSource`] wraps: variable name → value.
type Lookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Resolves a credential environment-variable NAME to its value.
///
/// Why: see the module doc — this is the seam that lets a test prove a
/// token-dependent branch without mutating the process environment.
/// What: a cloneable, thread-safe wrapper around a `&str -> Option<String>`
/// lookup, with the "an empty value means unset" rule applied once here rather
/// than re-spelled at each of the eight call sites that used to own it.
/// Test: `credential_source_*` in this module.
#[derive(Clone)]
pub(crate) struct CredentialSource {
    lookup: Lookup,
}

impl CredentialSource {
    /// The production lookup: the process environment.
    pub(crate) fn from_env() -> Self {
        Self {
            lookup: Arc::new(|name| std::env::var(name).ok()),
        }
    }

    /// A lookup that answers from a fixed `(name, value)` table and nothing
    /// else.
    ///
    /// Why: replaces the `set_var` / `remove_var` pair that used to bracket
    /// every credential-dependent test (#6405).
    /// What: unlisted names resolve to `None`, so a test also proves the
    /// "variable is unset" branch by simply not listing it.
    /// Test: `credential_source_answers_only_listed_names`.
    #[cfg(test)]
    pub(crate) fn fixed<K, V, I>(pairs: I) -> Self
    where
        K: Into<String>,
        V: Into<String>,
        I: IntoIterator<Item = (K, V)>,
    {
        let table: std::collections::HashMap<String, String> = pairs
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        Self {
            lookup: Arc::new(move |name| table.get(name).cloned()),
        }
    }

    /// A lookup where every variable is unset.
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self::fixed(Vec::<(String, String)>::new())
    }

    /// Resolve `name`, treating an empty value as unset.
    ///
    /// Why: an exported-but-empty credential var is a shell accident, never an
    /// intentional empty key — every caller already applied this rule, and
    /// applying it once here keeps them from drifting apart.
    /// What: returns the lookup's value for `name` unless it is the empty
    /// string.
    /// Test: `credential_source_treats_empty_as_unset`.
    pub(crate) fn get(&self, name: &str) -> Option<String> {
        (self.lookup)(name).filter(|v| !v.is_empty())
    }
}

impl Default for CredentialSource {
    fn default() -> Self {
        Self::from_env()
    }
}

impl std::fmt::Debug for CredentialSource {
    /// Never renders a value — a `Debug` of a credential source must not be a
    /// way to leak one into a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialSource(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_source_answers_only_listed_names() {
        let creds = CredentialSource::fixed([("TOKEN_A", "value-a")]);
        assert_eq!(creds.get("TOKEN_A").as_deref(), Some("value-a"));
        assert_eq!(creds.get("TOKEN_B"), None);
    }

    #[test]
    fn credential_source_treats_empty_as_unset() {
        let creds = CredentialSource::fixed([("TOKEN_A", "")]);
        assert_eq!(
            creds.get("TOKEN_A"),
            None,
            "an exported-but-empty value must read as unset"
        );
    }

    #[test]
    fn credential_source_empty_answers_nothing() {
        assert_eq!(CredentialSource::empty().get("ANYTHING"), None);
    }

    #[test]
    fn credential_source_from_env_reads_the_process_environment() {
        // PATH is set in every environment this suite runs in, and this test
        // only READS it — no mutation, which is the whole point of #6405.
        let creds = CredentialSource::from_env();
        assert_eq!(creds.get("PATH"), std::env::var("PATH").ok());
    }

    #[test]
    fn credential_source_debug_hides_the_lookup() {
        assert_eq!(
            format!("{:?}", CredentialSource::fixed([("T", "secret")])),
            "CredentialSource(..)"
        );
    }
}
