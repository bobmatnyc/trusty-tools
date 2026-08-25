//! Whether this machine can authenticate a `git fetch` without a prompt (#6244).
//!
//! Why: `tga audit` fetches every repository before it collects, and its fetch
//! resolves a credential non-interactively or not at all. On a machine with
//! none, every fetch fails, pull-request collection produces a header-only
//! `pr-metrics.csv`, and the sweep reports success — 59 repositories deep, the
//! only trace was the same one-line gap repeated in 59 separate manifests.
//! Nothing said it once, up front, where an operator would see it before
//! spending the hours.
//!
//! Two halves, and the second is the one that actually fixes the common case:
//!
//! 1. **The `gh` login reaches the fetch.** [`super::github_issues`] already
//!    reads `gh auth token` once per sweep, but hands it to the child as
//!    `TRUSTY_AUDIT_GITHUB_TOKEN` — a name only tga's `github:` config section
//!    references. tga's git transport reads `GITHUB_TOKEN` / `GH_TOKEN`, so a
//!    recipient logged in with `gh` and nothing else had a usable credential
//!    this crate held and never passed on. [`GitCredential::supplies_github_token`]
//!    is what closes that.
//! 2. **No credential at all is an up-front refusal**, not 59 empty CSVs. See
//!    [`refuse_if_fetching`].
//!
//! ## The source list mirrors tga's, deliberately
//!
//! The four sources below are `tga::collect::git::fetch`'s
//! `non_interactive_credentials`, in its order: SSH agent, `~/.ssh/id_ed25519`,
//! `~/.ssh/id_rsa`, `GITHUB_TOKEN`/`GH_TOKEN`, platform credential helper. This
//! module cannot call that function — it builds `git2::Cred` values inside the
//! fetch callback — so it answers the weaker question the preflight needs: is
//! there anything here at all for that callback to find. It is deliberately
//! OPTIMISTIC, and both directions of the gap are worth naming: a source
//! present here can still be REJECTED by the remote (an expired token, a key
//! the remote does not know), and the platform credential helper is not probed
//! at all, so a machine whose only credential lives in the keychain reads as
//! having none. That is why the refusal fires only when NOTHING is found and
//! the engagement provably needs a fetch.
//!
//! Test: `git_credential_tests`.

use std::path::{Path, PathBuf};

use crate::error::AuditError;

/// The HTTPS token variable tga's fetch reads first.
pub const ENV_GITHUB_TOKEN: &str = trusty_common::env_vars::ENV_GITHUB_TOKEN;

/// The `gh` CLI's own spelling of the same thing, which tga's fetch also reads.
pub const ENV_GH_TOKEN: &str = "GH_TOKEN";

/// Where an `ssh-agent` advertises itself.
const ENV_SSH_AGENT: &str = "SSH_AUTH_SOCK";

/// The two private keys tga's fetch tries by name, in its order.
const SSH_KEY_FILES: [&str; 2] = ["id_ed25519", "id_rsa"];

/// What this machine can authenticate a `git fetch` with, and from where.
///
/// Why: the sweep needs two different answers out of one resolution — whether
/// to refuse before spending hours, and whether it must hand its own `gh` token
/// to the child under the name the git transport reads. Resolving twice would
/// let those two disagree.
/// What: the named sources, in the order they were found, and whether the `gh`
/// login is the only one — which is exactly when this process has a credential
/// the child would not otherwise see.
/// Test: `git_credential_tests`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct GitCredential {
    sources: Vec<String>,
    from_gh_login: bool,
}

impl GitCredential {
    /// What the operator's own environment offers, before `gh` is consulted.
    ///
    /// `operator` is the injected environment lookup `super::sweep_with_env`
    /// already carries, so every arm is provable without `std::env::set_var` —
    /// `unsafe` in edition 2024 and unsound under the parallel harness.
    pub(crate) fn of_environment<F>(operator: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let mut sources = Vec::new();
        for name in [ENV_GITHUB_TOKEN, ENV_GH_TOKEN] {
            if nonblank(operator(name)).is_some() {
                sources.push(format!("{name} in this environment"));
            }
        }
        if nonblank(operator(ENV_SSH_AGENT)).is_some() {
            sources.push(format!("an SSH agent ({ENV_SSH_AGENT})"));
        }
        if let Some(home) = nonblank(operator("HOME")) {
            sources.extend(ssh_keys_under(Path::new(&home)));
        }
        Self {
            sources,
            from_gh_login: false,
        }
    }

    /// The same resolution, with the sweep's `gh auth token` result folded in.
    ///
    /// Why: `gh` is consulted LAST, matching tga's own order — an operator who
    /// exported a token meant that token, and overriding it with a keychain
    /// login they may not have thought about is not this crate's call. The
    /// consequence is [`Self::supplies_github_token`]: this process forwards the
    /// `gh` login only when nothing else answered, so it can add a credential
    /// the child would otherwise lack but never replace one it already has.
    #[must_use]
    pub(crate) fn with_gh_login(mut self, token: Option<&str>) -> Self {
        if nonblank(token.map(str::to_owned)).is_none() {
            return self;
        }
        self.from_gh_login = self.sources.is_empty();
        self.sources.push("your `gh` login".to_owned());
        self
    }

    /// Every credential source found, in the order they were looked for.
    pub(crate) fn sources(&self) -> &[String] {
        &self.sources
    }

    /// Whether this process must hand the child a `GITHUB_TOKEN` of its own.
    ///
    /// True exactly when the `gh` login is the only source: the child inherits
    /// this process's environment, so an operator-exported token already
    /// reaches it, while a token read out of `gh`'s keychain does not.
    pub(crate) fn supplies_github_token(&self) -> bool {
        self.from_gh_login
    }

    /// Refuse the sweep when nothing can authenticate a fetch it needs.
    ///
    /// # Errors
    ///
    /// [`AuditError::NoGitCredential`] when no source was found AND
    /// `fetching` names at least one repository. Both halves are required: an
    /// engagement over checkouts with no `github.com` remote fetches nothing, so
    /// refusing it would strand a legitimate run over a credential it never
    /// needed.
    /// Test: `git_credential_tests::{nothing_at_all_refuses_a_fetching_engagement,
    /// nothing_at_all_still_runs_an_engagement_that_fetches_nothing}`.
    pub(crate) fn refuse_if_fetching(&self, fetching: &[String]) -> Result<(), AuditError> {
        if !self.sources.is_empty() || fetching.is_empty() {
            return Ok(());
        }
        Err(AuditError::NoGitCredential {
            repositories: fetching.len(),
            first: fetching[0].clone(),
        })
    }
}

/// The private keys under `home/.ssh` that tga's fetch would try, named.
fn ssh_keys_under(home: &Path) -> Vec<String> {
    SSH_KEY_FILES
        .iter()
        .map(|name| (name, home.join(".ssh").join(name)))
        .filter(|(_, path)| path.is_file())
        .map(|(name, _)| format!("~/.ssh/{name}"))
        .collect()
}

/// A value with surrounding whitespace removed, or `None` when nothing is left.
///
/// A blank token is not a token: `gh auth token` under an unusable login exits
/// zero printing whitespace, which is the shape #5475 already had to guard.
fn nonblank(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// The selected repositories whose checkout names a `github.com` remote.
///
/// Why: the refusal must fire on the engagements that actually fetch and stay
/// silent on the ones that do not, and the checkout's own `origin` is the only
/// thing that answers that — not the registered name, which is `owner/name` for
/// a repository this client cloned and a bare basename for a path on disk.
/// What: [`crate::local_repo::github_slug`] per checkout, which is the crate's
/// one entry point for that read. A checkout `git` could not be run against at
/// all is left OUT: this list drives a refusal, and a refusal must not rest on
/// a question nothing managed to ask.
/// Test: `super::run_tests::a_sweep_over_checkouts_with_no_remote_needs_no_credential`.
pub(crate) async fn github_backed(checkouts: &[(String, PathBuf)]) -> Vec<String> {
    let mut named = Vec::new();
    for (name, path) in checkouts {
        if matches!(crate::local_repo::github_slug(path).await, Ok(Some(_))) {
            named.push(name.clone());
        }
    }
    named
}

#[cfg(test)]
mod git_credential_tests {
    use super::*;

    /// An environment lookup that answers for exactly one name.
    fn only(wanted: &'static str, value: &'static str) -> impl Fn(&str) -> Option<String> {
        move |name| (name == wanted).then(|| value.to_owned())
    }

    /// 🔴 #6244: the whole point. Nothing on the machine can authenticate a
    /// fetch, the engagement provably needs one, and the run must say so before
    /// it spends four hours producing 59 header-only `pr-metrics.csv` files and
    /// reporting success.
    ///
    /// Against `3771644d0` there was no preflight of any kind — the sweep
    /// started, every child's fetch failed, and the only trace was one gap line
    /// repeated inside 59 separate manifests.
    #[test]
    fn nothing_at_all_refuses_a_fetching_engagement() {
        let credential = GitCredential::of_environment(|_| None).with_gh_login(None);
        assert!(credential.sources().is_empty(), "{credential:?}");
        let err = credential
            .refuse_if_fetching(&["acme/api".to_owned(), "acme/web".to_owned()])
            .expect_err("a fetching engagement with no credential is refused");
        let message = err.to_string();
        for expected in ["acme/api", "gh auth login", ENV_GITHUB_TOKEN, "id_ed25519"] {
            assert!(message.contains(expected), "{message}");
        }
    }

    /// The other half of the gate: an engagement over checkouts with no
    /// `github.com` remote fetches nothing, so it must not be refused over a
    /// credential it never needed.
    #[test]
    fn nothing_at_all_still_runs_an_engagement_that_fetches_nothing() {
        GitCredential::of_environment(|_| None)
            .with_gh_login(None)
            .refuse_if_fetching(&[])
            .expect("nothing to fetch, nothing to refuse");
    }

    /// A token the operator exported already reaches the child, because the
    /// child inherits this process's environment — so it is a source, and this
    /// process adds nothing of its own.
    #[test]
    fn an_exported_token_is_a_source_this_process_need_not_forward() {
        for name in [ENV_GITHUB_TOKEN, ENV_GH_TOKEN] {
            let credential =
                GitCredential::of_environment(only(name, "t")).with_gh_login(Some("gh"));
            assert_eq!(credential.sources().len(), 2, "{credential:?}");
            assert!(
                !credential.supplies_github_token(),
                "an operator's own token must not be replaced: {credential:?}"
            );
            credential
                .refuse_if_fetching(&["acme/api".to_owned()])
                .expect("a source is a source");
        }
    }

    /// 🔴 #6244: a recipient logged in with `gh` and nothing else. The token
    /// exists, this crate already reads it, and before this it reached the child
    /// only under `TRUSTY_AUDIT_GITHUB_TOKEN` — a name tga's git transport does
    /// not read. That is the fetch failing while a usable credential sat in this
    /// process's own memory.
    #[test]
    fn a_gh_login_alone_is_forwarded_as_the_transport_token() {
        let credential = GitCredential::of_environment(|_| None).with_gh_login(Some("gho_x"));
        assert!(credential.supplies_github_token(), "{credential:?}");
        credential
            .refuse_if_fetching(&["acme/api".to_owned()])
            .expect("the gh login is a credential");
    }

    /// A blank `gh auth token` — the exit-zero-printing-whitespace shape — is
    /// not a credential, and must not silence the refusal.
    #[test]
    fn a_blank_gh_token_is_not_a_source() {
        let credential = GitCredential::of_environment(|_| None).with_gh_login(Some("  \n"));
        assert!(credential.sources().is_empty(), "{credential:?}");
        credential
            .refuse_if_fetching(&["acme/api".to_owned()])
            .expect_err("a blank token is no token");
    }

    /// An agent socket and a key file are each a source on their own.
    #[test]
    fn an_agent_or_a_key_file_is_a_source() {
        let agent = GitCredential::of_environment(only(ENV_SSH_AGENT, "/tmp/agent.sock"));
        assert_eq!(agent.sources().len(), 1, "{agent:?}");

        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".ssh")).expect("mkdir");
        std::fs::write(tmp.path().join(".ssh").join("id_ed25519"), "k").expect("write");
        let home = tmp.path().to_string_lossy().into_owned();
        let keyed = GitCredential::of_environment(|name| (name == "HOME").then(|| home.clone()));
        assert_eq!(keyed.sources(), ["~/.ssh/id_ed25519"], "{keyed:?}");
    }
}
