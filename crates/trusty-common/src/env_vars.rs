//! Canonical environment-variable name constants shared across the workspace.
//!
//! Why: the same credential env-var names (`OPENROUTER_API_KEY`, `GITHUB_TOKEN`)
//! were spelled as bare string literals at ~40 `std::env::var(...)` call sites
//! across nine crates. A typo in any one of them fails silently — the read just
//! returns `Err`/`None` and the feature quietly degrades. Naming each once here
//! makes the set authoritative and typo-proof: a wrong name is a compile error,
//! not a silent misread.
//! What: `&'static str` constants holding the exact env-var names. The values
//! are the literal names, so substituting a constant for a literal is a pure
//! zero-behavior rename — the resolved variable is byte-identical.
//! Test: `env_var_names_are_stable` pins the literal values.

/// The OpenRouter API-key environment variable (`OPENROUTER_API_KEY`).
///
/// Why: the deployment / CI fallback credential for LLM calls across the
/// inference, review, analyze, search, memory, agents, and code crates.
/// What: the exact env-var name; pass to `std::env::var`.
/// Test: `env_var_names_are_stable`.
pub const ENV_OPENROUTER_API_KEY: &str = "OPENROUTER_API_KEY";

/// The GitHub token environment variable (`GITHUB_TOKEN`).
///
/// Why: the token the ticketing, review, analyze, git-analytics, and installer
/// paths read to authenticate GitHub API calls.
/// What: the exact env-var name; pass to `std::env::var`.
/// Test: `env_var_names_are_stable`.
pub const ENV_GITHUB_TOKEN: &str = "GITHUB_TOKEN";

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the literal env-var names so a rename here can never silently change
    /// which variable every call site reads.
    #[test]
    fn env_var_names_are_stable() {
        assert_eq!(ENV_OPENROUTER_API_KEY, "OPENROUTER_API_KEY");
        assert_eq!(ENV_GITHUB_TOKEN, "GITHUB_TOKEN");
    }
}
