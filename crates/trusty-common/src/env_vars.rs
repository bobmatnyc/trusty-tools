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

/// Investigation file budget an audit asks its renderer for
/// (`TRUSTY_AUDIT_INVESTIGATE_MAX_FILES`).
///
/// Why (#6082): `trusty-audit` writes this budget into the manifest, but on the
/// sweep path the manifest edit lands after the `tga audit` child has already
/// run `trusty-review report` — so the renderer read its own 40-file default and
/// the audit's 240 never arrived. The environment reaches the grandchild before
/// the file does, so the same number travels both ways and the two spellings
/// have to agree; a literal in each crate is the drift this module exists to
/// stop.
/// What: the exact env-var name; pass to `std::env::var`. Read by
/// `trusty_audit::grounding::priority::Budget` and by `trusty-review`'s
/// `resolve_budget`, in both cases BELOW an explicit flag and below the
/// manifest key.
/// Test: `env_var_names_are_stable`.
pub const ENV_AUDIT_INVESTIGATE_MAX_FILES: &str = "TRUSTY_AUDIT_INVESTIGATE_MAX_FILES";

/// Investigation byte budget an audit asks its renderer for
/// (`TRUSTY_AUDIT_INVESTIGATE_MAX_BYTES`).
///
/// Why/What/Test: the byte half of [`ENV_AUDIT_INVESTIGATE_MAX_FILES`], resolved
/// under the same precedence.
pub const ENV_AUDIT_INVESTIGATE_MAX_BYTES: &str = "TRUSTY_AUDIT_INVESTIGATE_MAX_BYTES";

/// Report template an audit asks its renderer for
/// (`TRUSTY_AUDIT_REPORT_TEMPLATE`).
///
/// Why (#6669): `trusty-audit`'s `[report] template` key has to reach
/// `trusty-review report`, and on the sweep path it reaches it through `tga
/// audit` — a grandchild this crate never spawns and whose argument vector it
/// does not own. The environment crosses both process boundaries. An argument
/// would also break an older pinned renderer, which exits 2 on a flag it does
/// not know rather than ignoring it.
/// What: the exact env-var name; the value is a template name or the `cast`
/// alias. Read by `trusty-review`'s report entry point BELOW the `--template`
/// flag and below the manifest `[report].template` key.
/// Test: `env_var_names_are_stable`.
pub const ENV_AUDIT_REPORT_TEMPLATE: &str = "TRUSTY_AUDIT_REPORT_TEMPLATE";

/// Code-only rendering an audit asks its renderer for
/// (`TRUSTY_AUDIT_REPORT_CODE_ONLY`).
///
/// Why/What/Test: the boolean half of [`ENV_AUDIT_REPORT_TEMPLATE`], resolved
/// under the same precedence. `1`/`true`/`yes`/`on` (case-insensitive) enable
/// it; anything else reads as absent, so a typo never silently narrows what a
/// report says its scope was.
pub const ENV_AUDIT_REPORT_CODE_ONLY: &str = "TRUSTY_AUDIT_REPORT_CODE_ONLY";

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the literal env-var names so a rename here can never silently change
    /// which variable every call site reads.
    #[test]
    fn env_var_names_are_stable() {
        assert_eq!(ENV_OPENROUTER_API_KEY, "OPENROUTER_API_KEY");
        assert_eq!(ENV_GITHUB_TOKEN, "GITHUB_TOKEN");
        assert_eq!(
            ENV_AUDIT_INVESTIGATE_MAX_FILES,
            "TRUSTY_AUDIT_INVESTIGATE_MAX_FILES"
        );
        assert_eq!(
            ENV_AUDIT_INVESTIGATE_MAX_BYTES,
            "TRUSTY_AUDIT_INVESTIGATE_MAX_BYTES"
        );
        // #6669: the CAST code-only selection travels the same way, for the
        // same reason — see the two constants' own docs.
        assert_eq!(ENV_AUDIT_REPORT_TEMPLATE, "TRUSTY_AUDIT_REPORT_TEMPLATE");
        assert_eq!(ENV_AUDIT_REPORT_CODE_ONLY, "TRUSTY_AUDIT_REPORT_CODE_ONLY");
    }
}
