//! What every registered repository automatically contributes: its GitHub
//! issues (#5980).
//!
//! Why: owner ruling 2026-08-18 — "when a repository is registered as a
//! target, collect from its GitHub issues automatically... No separate board
//! registration for the repo's own issue tracker." Unlike a JIRA project or a
//! Linear team ([`super::boards`]), a repository's issue tracker is not
//! something an operator registers separately: every [`crate::registry::Target::Repo`]
//! already names the repository, and that name is also the identity tga's
//! `github.repo` field needs. So this module has no `resolve(targets, …)`
//! step of its own — [`GithubAccess::section`] is called once per selected
//! repository, directly from the same name `crate::run::SelectedRepo::name`
//! already carries (which [`crate::clone::record_selection`] sets from
//! [`crate::registry::Target::Repo::name_with_owner`], so it is already
//! `owner/name`, never a display-only name).
//!
//! ## Auth: `gh`'s own OAuth, never a second token field
//!
//! Owner ruling: "for the boards, we should be using oauth… same with gh."
//! [`resolve_github_access`] reuses [`crate::discover::credential_probe`] —
//! the SAME `gh auth token` invocation `crate::clone` and `crate::validate`
//! already run — rather than adding a `boards.github` (or similar) config
//! field the recipient would have to populate with a raw PAT. A `gh` that
//! cannot answer is not fatal: [`GithubAccess::token`] stays `None`, tga runs
//! the REST client unauthenticated, and a repository that needs auth to read
//! its issues fails at the API and is reported the way any other
//! `work_items` fetch failure is — see the fail-open note below.
//!
//! ## Fail-open: no gap invented here, because tga already states one
//!
//! DOC-67 §9 / tga #5655 already turns ANY `work_items` fetch fault — issues
//! disabled (HTTP 410), a rejected credential (401/403), or an unreachable
//! endpoint — into a failed `collect` stage, which `sweep_gap_lines` turns
//! into a named line in that repository's own manifest gaps
//! (`crate::run::verify_output` reads it into `crate::run::RepoRun::gaps`).
//! That is the JIRA precedent #5980 asks this feature to match: a stage that
//! failed must not read as a stage that produced nothing. Because that
//! mechanism already exists and already fires once `github:` is present in
//! the generated config, this module's only obligation is to make sure that
//! section is ALWAYS generated for every registered repository — never
//! omitted because a token could not be read. Omitting it on a credential
//! failure would be the silent-empty-success shape #5982 already fixed for
//! Linear, repeated here under a new name.
//!
//! Test: `github_issues_tests` below.

use serde::Serialize;

/// The variable the `gh`-derived token reaches the child under.
pub const ENV_GITHUB_TOKEN: &str = "TRUSTY_AUDIT_GITHUB_TOKEN";

/// tga's `github:` section, as this client generates it.
///
/// Enough to drive `work_item_pipeline`'s reference-detection pull (`#NNNN` in
/// commit messages) — not pull-request collection, which this client does not
/// ask tga for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TgaGithub {
    /// `owner/name` — the same identity the repository was registered and
    /// cloned under.
    pub repo: String,
    /// A `${VAR}` reference to the child's environment, never the token
    /// itself — see the module docs. `None` when no `gh` credential could be
    /// read; tga then runs unauthenticated rather than the section being
    /// omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// What one sweep could read from `gh` for issue collection, resolved once.
///
/// Why: every child gets the same credential — it is the recipient's own
/// `gh` login, not a per-repository secret — so reading it once and reusing
/// it matches how `crate::inference::inference_env` is resolved once per
/// sweep rather than per repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GithubAccess {
    /// The real token value, if `gh auth token` answered. Never written to
    /// disk — only exposed into a child's environment, the same shape as
    /// `super::boards::Boards::env`.
    token: Option<String>,
}

impl GithubAccess {
    /// The `(name, value)` pair to set in a child's environment, when a
    /// credential was read.
    pub fn env(&self) -> Option<(&'static str, &str)> {
        self.token.as_deref().map(|t| (ENV_GITHUB_TOKEN, t))
    }

    /// This repository's `github:` section — always `Some`, per the module
    /// docs: a repository target always contributes its issues, and a
    /// missing credential is not a reason to omit the attempt.
    ///
    /// # Postconditions
    /// `repo` equals `name_with_owner` verbatim; `token` is `Some("${…}")`
    /// (never the real value) exactly when this access resolved one.
    /// Test: `github_issues_tests::a_repo_with_a_token_gets_the_placeholder_never_the_value`,
    /// `github_issues_tests::a_repo_with_no_token_still_gets_a_github_section`.
    pub fn section(&self, name_with_owner: &str) -> TgaGithub {
        TgaGithub {
            repo: name_with_owner.to_owned(),
            token: self
                .token
                .as_ref()
                .map(|_| format!("${{{ENV_GITHUB_TOKEN}}}")),
        }
    }
}

/// Resolve the `gh`-derived credential once per sweep.
///
/// What: [`crate::discover::credential_probe`]'s `gh auth token`, trimmed.
/// `Err` (no `gh`, not logged in, blank token) becomes [`GithubAccess`] with
/// no token rather than a sweep-ending failure — see the module docs for why
/// that is safe.
/// Test: `github_issues_tests::an_err_result_resolves_to_no_token`,
/// `github_issues_tests::an_ok_result_is_carried_through`.
pub async fn resolve_github_access() -> GithubAccess {
    from_probe_result(crate::discover::credential_probe().nonempty_stdout().await)
}

/// [`Result`] of [`GithubAccess`] from an already-resolved outcome.
///
/// Why: a test proving the no-token fallback needs to drive both arms of
/// [`resolve_github_access`]'s match WITHOUT spawning a real `gh` — the same
/// offline-provable shape `crate::validate::RepoProbe::unusable` gives the
/// repository-validation arm (#5822). Rather than injecting a fake process
/// spawner, this takes the already-resolved `Result` the real function
/// would have awaited, so the mapping is exercised directly.
/// Test: `github_issues_tests::{an_err_result_resolves_to_no_token,
/// an_ok_result_is_carried_through}`.
fn from_probe_result(result: Result<String, trusty_common::gh::GhError>) -> GithubAccess {
    match result {
        Ok(token) => GithubAccess { token: Some(token) },
        Err(_) => GithubAccess::default(),
    }
}

#[cfg(test)]
mod github_issues_tests {
    use super::*;

    #[test]
    fn a_repo_with_a_token_gets_the_placeholder_never_the_value() {
        let access = GithubAccess {
            token: Some("ghp_real-token-do-not-write-me-down".to_owned()),
        };
        let section = access.section("acme/api");
        assert_eq!(section.repo, "acme/api");
        assert_eq!(
            section.token.as_deref(),
            Some("${TRUSTY_AUDIT_GITHUB_TOKEN}")
        );
        assert_eq!(
            access.env(),
            Some((ENV_GITHUB_TOKEN, "ghp_real-token-do-not-write-me-down"))
        );
    }

    /// #5980: the load-bearing property — an unreadable credential must not
    /// make the section disappear, or a repository whose token expired would
    /// silently stop contributing issues with nothing said anywhere. `token:
    /// None` lets tga run unauthenticated instead of the section vanishing.
    #[test]
    fn a_repo_with_no_token_still_gets_a_github_section() {
        let access = GithubAccess::default();
        let section = access.section("acme/api");
        assert_eq!(section.repo, "acme/api");
        assert_eq!(section.token, None);
        assert_eq!(access.env(), None);
    }

    #[test]
    fn the_generated_document_omits_the_token_key_entirely_when_absent() {
        let section = GithubAccess::default().section("acme/api");
        let yaml = serde_yaml::to_string(&section).expect("serializes");
        assert!(!yaml.contains("token"), "{yaml}");
        assert!(yaml.contains("acme/api"), "{yaml}");
    }

    /// A `gh` that cannot answer (not installed, not logged in) must not stop
    /// the sweep or panic — it resolves to no token, and every repository
    /// still gets a `github:` section per the property above.
    #[test]
    fn an_err_result_resolves_to_no_token() {
        let access = from_probe_result(Err(trusty_common::gh::GhError::NotInstalled));
        assert_eq!(access.token, None);
    }

    #[test]
    fn an_ok_result_is_carried_through() {
        let access = from_probe_result(Ok("a-real-token".to_owned()));
        assert_eq!(access.token.as_deref(), Some("a-real-token"));
    }
}
