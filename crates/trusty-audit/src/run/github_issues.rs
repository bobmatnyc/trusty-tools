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
//! Corrected (#5980 CRITICAL 2): the section below used to credit DOC-67 §9
//! for this mechanism; that section is about `report --analyze`'s
//! `HttpAnalyzeMetricsSource` enrichment, not about `work_items` collection —
//! wrong governing spec, kept here only as the record of the mistake.
//!
//! tga #5655 turns ANY `work_items` fetch fault — issues disabled (HTTP
//! 410), a rejected credential (401/403), or an unreachable endpoint — into a
//! failed `collect` stage, which `tga::audit::gaps::sweep_gap_lines` turns
//! into a named line in that repository's own manifest gaps
//! (`crate::run::verify_output` reads it into `crate::run::RepoRun::gaps`).
//! **That chain depends on the fetch actually erroring**, though: before
//! #5980 CRITICAL 1's fix, `GitHubClient::fetch_issue` mapped a private
//! repository's every-request-404 to `Ok(None)` — indistinguishable from "no
//! such issue" — so `work_item_pipeline::run_one_adapter`'s authoritative
//! not-found arm recorded nothing and #5655's mechanism never fired at all.
//! The repo-visibility probe added there is what makes an invisible repo
//! reach `stats.fail_stage`/`skip_item` in the first place; this module's
//! job is unchanged — making sure `github:` is present in the generated
//! config for every registered repository, never omitted because a token
//! could not be read, so that when the fetch below it fails, the failure has
//! something to attach to. Omitting the section on a credential failure
//! would be the silent-empty-success shape #5982 already fixed for Linear,
//! repeated here under a new name. That is the JIRA precedent #5980 asks
//! this feature to match: a stage that failed must not read as a stage that
//! produced nothing.
//!
//! Test: `github_issues_tests` below.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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

    /// The real token value, for the outbound credential guards ONLY (#5980
    /// CRITICAL 3 / CRITICAL 4) — every other consumer gets [`Self::env`]'s
    /// child-environment pair or [`Self::section`]'s `${VAR}` placeholder,
    /// never this.
    ///
    /// Why: a child that echoes an auth failure back (a rejected `gh`-derived
    /// token quoted in an HTTP error body) can put the raw value in its own
    /// stdout/stderr, or in a file it writes under `out/`/`extract/` — the
    /// same two places `crate::run`'s child-log scrubber and
    /// `crate::package`'s outbound scan already guard for `boards`' JIRA and
    /// Linear credentials (#5857). Neither guard could see this token before
    /// it had a way to read it back out of [`GithubAccess`].
    /// What: the raw `Option<&str>`, unmodified.
    /// Test: `github_issues_tests::raw_token_exposes_the_value_the_placeholder_hides`.
    pub(crate) fn raw_token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// A non-reversible, truncated identity fingerprint of the resolved
    /// token, or `None` when no credential was resolved.
    ///
    /// Why (#5980 — the re-resolution/account-switch gap): packaging
    /// re-resolves `gh auth token` fresh rather than inheriting the sweep's
    /// value (see [`verify_unchanged`]'s callers), because packaging can run
    /// as a separate process — possibly after the operator switched `gh`
    /// accounts between the sweep and the package command. Scanning outbound
    /// files with the NEW token's bytes can never catch an OLD token a child
    /// echoed into a file at sweep time. This fingerprint is what lets
    /// packaging PROVE the two resolutions are the same account without ever
    /// persisting the raw token — [`crate::run::checkpoint::RunProgress`]
    /// stores only this value.
    /// What: the first 8 bytes (16 hex characters) of the token's SHA-256
    /// digest. Truncated deliberately: this exists to detect a DIFFERENT
    /// token, never to be treated as a secondary secret worth protecting at
    /// full strength.
    /// Test: `github_issues_tests::{fingerprint_is_stable_for_the_same_token,
    /// fingerprint_differs_for_different_tokens, fingerprint_is_none_with_no_token}`.
    pub(crate) fn fingerprint(&self) -> Option<String> {
        self.token.as_deref().map(|t| {
            let digest = Sha256::digest(t.as_bytes());
            digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
        })
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

    /// Build a [`GithubAccess`] carrying a specific token, for tests that
    /// need to prove the credential reaches a child's environment or an
    /// outbound guard's needle set without spawning a real `gh` (#5980
    /// MEDIUM 1). Production code only ever constructs this type through
    /// [`resolve_github_access`].
    #[cfg(test)]
    pub(crate) fn with_token(token: impl Into<String>) -> Self {
        Self {
            token: Some(token.into()),
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

/// What a sweep recorded about the `gh`-derived credential it resolved.
///
/// Why: persisted into `crate::run::checkpoint::RunProgress` so packaging — a
/// later, possibly separate, process — can prove it is scanning outbound
/// files with the credential that produced them (see
/// [`GithubAccess::fingerprint`]'s docs for the gap this closes). The record
/// being ABSENT from a checkpoint (`Option::None` one level up, in
/// `RunProgress::github_credential`) is reserved for one specific case: a
/// checkpoint written before this field existed. This enum's own two
/// variants are for a sweep that recorded something, one way or the other —
/// [`verify_unchanged`] treats all three states differently.
/// Test: `github_issues_tests`, `verify_unchanged`'s own tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GithubCredentialRecord {
    /// The sweep resolved no `gh` credential and ran unauthenticated.
    NoToken,
    /// [`GithubAccess::fingerprint`] of the token the sweep resolved.
    Fingerprint(String),
}

impl GithubCredentialRecord {
    /// What a sweep should record, from the access it resolved.
    pub(crate) fn of(access: &GithubAccess) -> Self {
        match access.fingerprint() {
            Some(fp) => Self::Fingerprint(fp),
            None => Self::NoToken,
        }
    }
}

/// Confirm the currently-resolved credential is the one the sweep recorded,
/// before packaging trusts its scrub needle to have covered every file.
///
/// Why: see [`GithubAccess::fingerprint`]'s docs for the account-switch gap
/// this closes. Called from `crate::package::from_checkpoint`, which is the
/// one place that has both the checkpoint's record and a freshly resolved
/// [`GithubAccess`] in hand.
///
/// # Returns
/// `Ok(None)` when the two agree — including "neither run had a token",
/// which is always safe regardless of what the current resolution holds,
/// because a sweep with no token could not have put one in any file.
/// `Ok(Some(gap))` when `persisted` is `None` — the checkpoint predates this
/// field, so there is nothing to compare against; packaging proceeds, and the
/// returned line names the uncertainty for the operator.
/// `Err(AuditError::GithubCredentialChanged)` when the checkpoint recorded a
/// fingerprint this resolution provably disagrees with, whether the current
/// resolution has a different token or none at all — a fingerprint is
/// one-way, so a missing current token leaves no way to scan for the one the
/// checkpoint named.
/// Test: `github_issues_tests::{a_matching_fingerprint_proceeds_cleanly,
/// both_runs_having_no_token_proceeds_cleanly,
/// an_unrecorded_checkpoint_proceeds_with_a_gap,
/// a_mismatched_fingerprint_refuses,
/// losing_the_token_between_sweep_and_package_refuses}`.
///
/// # Errors
///
/// [`crate::error::AuditError::GithubCredentialChanged`] on a proven mismatch.
pub(crate) fn verify_unchanged(
    persisted: Option<&GithubCredentialRecord>,
    resolved: &GithubAccess,
) -> Result<Option<String>, crate::error::AuditError> {
    use crate::error::AuditError;
    match persisted {
        None => Ok(Some(
            "the GitHub credential used during collection was not recorded (this \
             checkpoint predates credential verification) — GitHub-derived content in \
             this deliverable could not be re-checked against the currently active \
             credential"
                .to_owned(),
        )),
        Some(GithubCredentialRecord::NoToken) => Ok(None),
        Some(GithubCredentialRecord::Fingerprint(old)) => match resolved.fingerprint() {
            Some(new) if new == *old => Ok(None),
            _ => Err(AuditError::GithubCredentialChanged),
        },
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

    /// #5980 MEDIUM 1: `section()` and `env()` both hide the real value behind
    /// a placeholder or a not-yet-dereferenced pair; `raw_token` is the one
    /// path that hands back the value itself, and only the outbound guards
    /// (the child-log scrubber, the package's credential scan) may call it.
    #[test]
    fn raw_token_exposes_the_value_the_placeholder_hides() {
        let access = GithubAccess::with_token("ghp_real-token-do-not-write-me-down");
        assert_eq!(
            access.raw_token(),
            Some("ghp_real-token-do-not-write-me-down")
        );
        assert_eq!(GithubAccess::default().raw_token(), None);
    }

    #[test]
    fn fingerprint_is_stable_for_the_same_token() {
        let a = GithubAccess::with_token("ghp_account_a_token");
        let b = GithubAccess::with_token("ghp_account_a_token");
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert!(a.fingerprint().is_some());
    }

    #[test]
    fn fingerprint_differs_for_different_tokens() {
        let a = GithubAccess::with_token("ghp_sweep_time_account_A_token");
        let b = GithubAccess::with_token("ghp_package_time_account_B_token");
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn fingerprint_is_none_with_no_token() {
        assert_eq!(GithubAccess::default().fingerprint(), None);
    }

    /// #5980 follow-up CRITICAL: a matching fingerprint proceeds with no gap.
    #[test]
    fn a_matching_fingerprint_proceeds_cleanly() {
        let access = GithubAccess::with_token("ghp_same_account_both_times");
        let persisted = GithubCredentialRecord::of(&access);
        let resolved = GithubAccess::with_token("ghp_same_account_both_times");
        assert_eq!(
            verify_unchanged(Some(&persisted), &resolved).expect("must not refuse"),
            None
        );
    }

    /// A sweep that ran with no token can never have leaked one — whatever is
    /// active at package time, there is nothing on disk to have caught.
    #[test]
    fn both_runs_having_no_token_proceeds_cleanly() {
        let persisted = GithubCredentialRecord::NoToken;
        assert_eq!(
            verify_unchanged(Some(&persisted), &GithubAccess::default()).expect("no token twice"),
            None
        );
        let with_new_token = GithubAccess::with_token("ghp_a_token_that_showed_up_later");
        assert_eq!(
            verify_unchanged(Some(&persisted), &with_new_token)
                .expect("a sweep with no token never leaked one"),
            None
        );
    }

    /// The back-compat case: a checkpoint written before this field existed.
    /// Packaging proceeds — refusing every pre-existing engagement's checkpoint
    /// outright would be disproportionate to a narrowly-scoped verification —
    /// but the gap line says so, rather than silently claiming agreement.
    #[test]
    fn an_unrecorded_checkpoint_proceeds_with_a_gap() {
        let gap = verify_unchanged(None, &GithubAccess::default())
            .expect("an unrecorded checkpoint is not a refusal");
        assert!(gap.is_some(), "the uncertainty must be stated");
        assert!(gap.unwrap().contains("not recorded"));
    }

    /// #5980 follow-up CRITICAL — the critic's proof, inverted: a file
    /// carrying the sweep-time account's token, packaged under a DIFFERENT
    /// account's token, must now refuse instead of silently succeeding.
    #[test]
    fn a_mismatched_fingerprint_refuses() {
        let sweep_time = GithubAccess::with_token("ghp_sweep_time_account_A_token");
        let persisted = GithubCredentialRecord::of(&sweep_time);
        let package_time = GithubAccess::with_token("ghp_package_time_account_B_token");
        assert!(matches!(
            verify_unchanged(Some(&persisted), &package_time),
            Err(crate::error::AuditError::GithubCredentialChanged)
        ));
    }

    /// The one-way direction of the mismatch: a token recorded at sweep time
    /// but NO token active at package time is exactly as unverifiable as a
    /// different token — the fingerprint cannot be reversed into a needle.
    #[test]
    fn losing_the_token_between_sweep_and_package_refuses() {
        let sweep_time = GithubAccess::with_token("ghp_sweep_time_account_A_token");
        let persisted = GithubCredentialRecord::of(&sweep_time);
        assert!(matches!(
            verify_unchanged(Some(&persisted), &GithubAccess::default()),
            Err(crate::error::AuditError::GithubCredentialChanged)
        ));
    }
}
