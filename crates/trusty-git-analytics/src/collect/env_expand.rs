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
    // #5313: read the process env here so the rule below stays pure.
    expand_env_var_with(raw, |name| std::env::var(name).ok())
}

/// [`expand_env_var`], with the variable lookup supplied by the caller.
///
/// Why (#5313): proving that `${NAME}` resolves to the variable's value used to
/// require setting that variable process-wide — `unsafe` under the 2024 edition,
/// and a data race against every other thread `cargo test` runs in parallel.
/// A caller-supplied lookup makes every case provable without global state.
/// What: identical to [`expand_env_var`] except that `lookup` replaces the
/// `std::env::var` call; a `None` lookup result yields the empty string.
/// Test: the four `expand_env_var_*` tests below.
fn expand_env_var_with(raw: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    if raw.starts_with("${") && raw.ends_with('}') && raw.len() > 3 {
        let var = &raw[2..raw.len() - 1];
        if !var.is_empty() {
            return lookup(var).unwrap_or_default();
        }
    }
    raw.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lookup that resolves exactly one variable name and nothing else.
    ///
    /// #5313: replaces the `std::env::set_var` these tests used to call.
    fn only(name: &'static str, value: &'static str) -> impl Fn(&str) -> Option<String> {
        move |n| (n == name).then(|| value.to_string())
    }

    /// A lookup where every variable is unset.
    fn nothing_set(_: &str) -> Option<String> {
        None
    }

    /// Plain string with no placeholder syntax passes through unchanged, and
    /// the lookup is never consulted.
    #[test]
    fn expand_env_var_passthrough() {
        let boom = |n: &str| panic!("lookup should not run for {n}");
        assert_eq!(
            expand_env_var_with("ghp_actualtoken", boom),
            "ghp_actualtoken"
        );
        assert_eq!(expand_env_var_with("", boom), "");
        assert_eq!(
            expand_env_var_with("no-special-chars", boom),
            "no-special-chars"
        );
    }

    /// `${VAR}` whose value is set resolves to that value.
    #[test]
    fn expand_env_var_placeholder() {
        assert_eq!(
            expand_env_var_with("${GITHUB_TOKEN}", only("GITHUB_TOKEN", "resolved-value")),
            "resolved-value"
        );
    }

    /// `${VAR}` for an unset variable returns the empty string (not the
    /// literal placeholder), so callers can detect a missing credential.
    #[test]
    fn expand_env_var_unset_var() {
        assert_eq!(
            expand_env_var_with("${GITHUB_TOKEN}", nothing_set),
            "",
            "unset var should expand to empty string, not the literal placeholder"
        );
    }

    /// Strings that look like partial placeholders are passed through as-is,
    /// without consulting the lookup.
    #[test]
    fn expand_env_var_partial_placeholder() {
        let boom = |n: &str| panic!("lookup should not run for {n}");
        // Missing closing brace — no match, returned as-is.
        assert_eq!(expand_env_var_with("${NOCLOSE", boom), "${NOCLOSE");
        // Missing opening / dollar.
        assert_eq!(expand_env_var_with("VAR}", boom), "VAR}");
        assert_eq!(expand_env_var_with("$VAR", boom), "$VAR");
        // Empty name `${}` — passes through unchanged.
        assert_eq!(expand_env_var_with("${}", boom), "${}");
    }
}
