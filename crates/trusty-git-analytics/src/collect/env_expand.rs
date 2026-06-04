//! Shared `${ENV_VAR}` placeholder expansion for config values.
//!
//! Every provider client that reads credentials from config (GitHub, Linear,
//! Bitbucket, Azure DevOps) should run its token/key through
//! [`expand_env_var`] before use so that YAML values like
//! `token: "${GITHUB_TOKEN}"` work without manual pre-processing.

/// Resolve a `${VAR_NAME}` placeholder against the process environment.
///
/// Why: config files often store credentials as `${ENV_VAR}` references so
/// secrets stay out of YAML on disk; previously only the Linear client
/// expanded these, causing GitHub (and other providers) to pass the literal
/// placeholder string as a Bearer token, causing 401 Unauthorized errors
/// (issue #741).
/// What: if `raw` has the exact form `${NAME}` (non-empty `NAME`), returns
/// `std::env::var(NAME)` (empty string when the var is unset); otherwise
/// returns `raw` unchanged.
/// Test: `expand_env_var_placeholder`, `expand_env_var_passthrough`,
/// `expand_env_var_unset_var`, `expand_env_var_partial_placeholder` below.
pub fn expand_env_var(raw: &str) -> String {
    if raw.starts_with("${") && raw.ends_with('}') && raw.len() > 3 {
        let var = &raw[2..raw.len() - 1];
        if !var.is_empty() {
            return std::env::var(var).unwrap_or_default();
        }
    }
    raw.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plain string with no placeholder syntax passes through unchanged.
    #[test]
    fn expand_env_var_passthrough() {
        assert_eq!(expand_env_var("ghp_actualtoken"), "ghp_actualtoken");
        assert_eq!(expand_env_var(""), "");
        assert_eq!(expand_env_var("no-special-chars"), "no-special-chars");
    }

    /// `${VAR}` whose value is set in the environment resolves to that value.
    #[test]
    fn expand_env_var_placeholder() {
        std::env::set_var("TGA_TEST_TOKEN_741", "resolved-value");
        assert_eq!(expand_env_var("${TGA_TEST_TOKEN_741}"), "resolved-value");
        std::env::remove_var("TGA_TEST_TOKEN_741");
    }

    /// `${VAR}` for an unset variable returns the empty string (not the
    /// literal placeholder), so callers can detect a missing credential.
    #[test]
    fn expand_env_var_unset_var() {
        std::env::remove_var("TGA_TEST_DEFINITELY_UNSET_741");
        assert_eq!(
            expand_env_var("${TGA_TEST_DEFINITELY_UNSET_741}"),
            "",
            "unset var should expand to empty string, not the literal placeholder"
        );
    }

    /// Strings that look like partial placeholders are passed through as-is.
    #[test]
    fn expand_env_var_partial_placeholder() {
        // Missing closing brace — no match, returned as-is.
        assert_eq!(expand_env_var("${NOCLOSE"), "${NOCLOSE");
        // Missing opening / dollar.
        assert_eq!(expand_env_var("VAR}"), "VAR}");
        assert_eq!(expand_env_var("$VAR"), "$VAR");
        // Empty name `${}` — passes through unchanged.
        assert_eq!(expand_env_var("${}"), "${}");
    }
}
