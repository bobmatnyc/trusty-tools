//! Listing the repositories the recipient's credential can actually reach.
//!
//! Why: the picker (#5497) and the clone step (#5215) both need an answer to
//! "which repositories may this engagement audit?", and until now
//! [`crate::session::Command::Repos`] could only re-read a manifest a previous
//! run had already written — there was no way to produce that manifest in the
//! first place. #5487 is that gap. DOC-68 §6 and §14 Q4 (decided 2026-08-12)
//! settle the mechanism: route discovery through `gh`, not through a second
//! GitHub API client, because #5476 already makes `gh` the on-machine
//! credential holder for this flow and a second credential surface would
//! duplicate a capability `gh` already owns.
//!
//! What: [`discover`] enumerates the authenticated user's own repositories plus
//! every organization that account belongs to, then merges and de-duplicates
//! them. Every `gh` invocation goes through `trusty_common::gh::GhCommand`
//! (#5475) — this module spells `Command::new("gh")` nowhere.
//!
//! **Nothing here is allowed to answer "none" for a reason that is really
//! "failed".** A picker rendered from a silently-truncated list looks identical
//! to one rendered from a complete list, so the recipient would exclude
//! repositories from the audit without ever being told. Three specific guards:
//!
//! - The credential is probed with `nonempty_stdout`, because a whitespace
//!   `GH_TOKEN` makes `gh auth token` exit ZERO with blank stdout.
//! - `--limit` is always passed explicitly: `gh repo list` defaults to 30, and
//!   an account with 40 repositories would otherwise lose 10 with no signal.
//! - [`merge`] propagates the FIRST owner that failed rather than dropping it,
//!   so one unreadable org is an error, never a shorter list.
//!
//! Test: `super::discover_tests`, plus the `#[ignore]`d live probe there.

use serde::Deserialize;
use trusty_common::gh::{GhCommand, GhError};

use crate::error::AuditError;

/// How many repositories to ask `gh repo list` for per owner.
///
/// `gh repo list`'s own default is 30. Discovery that silently stops at 30 is
/// the failure this constant exists to prevent (#5487).
pub const DEFAULT_LIMIT: u32 = 1000;

/// Page size for the organization lookup. Accounts past this are not listed.
pub const ORG_PAGE_SIZE: u32 = 100;

/// One repository the recipient's credential can see.
///
/// Why: this is what the picker renders and what [`crate::clone`]-style work
/// will later clone, so it carries the identity (`name_with_owner`) that both
/// `gh repo clone` and a manifest entry need, plus the two flags an auditor
/// wants before selecting — private repositories are the interesting ones, and
/// archived repositories are usually noise.
/// What: the subset of `gh repo list --json` this crate reads. Field names are
/// `gh`'s own, mapped from camelCase.
/// Test: `super::discover_tests::a_gh_repo_list_payload_parses`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DiscoveredRepo {
    /// `owner/name` — the identity `gh repo clone` takes.
    pub name_with_owner: String,
    /// The bare repository name, without the owner.
    pub name: String,
    /// Whether the repository is private.
    #[serde(default)]
    pub is_private: bool,
    /// Whether the repository is archived.
    #[serde(default)]
    pub is_archived: bool,
    /// Web URL, for a picker that wants to link out.
    #[serde(default)]
    pub url: String,
}

/// The `--json` field set requested from `gh repo list`.
const REPO_FIELDS: &str = "nameWithOwner,name,isPrivate,isArchived,url";

#[derive(Debug, Clone, Deserialize)]
struct Org {
    login: String,
}

/// The credential probe, as a command rather than a run.
///
/// Why: `nonempty_stdout` rather than `stdout` is the whole point — `gh auth
/// token` exits non-zero when no account is logged in, but a whitespace
/// `GH_TOKEN` supplied through the environment exits ZERO printing whitespace,
/// and a caller reading only the status would proceed to list repositories with
/// no usable credential and get an empty list back (#5475, #5487).
/// What: `gh auth token`, with `GH_REPO` stripped so an ambient override cannot
/// redirect the probe.
/// Test: `super::discover_tests::the_credential_probe_asks_gh_for_a_token`.
fn credential_probe() -> GhCommand {
    GhCommand::new(["auth", "token"]).env_remove("GH_REPO")
}

/// The repository listing for one owner, as a command rather than a run.
///
/// `owner: None` lists the authenticated user's own repositories; `Some(login)`
/// lists that organization's.
///
/// Test: `super::discover_tests::the_listing_always_passes_an_explicit_limit`.
fn list_command(owner: Option<&str>, limit: u32) -> GhCommand {
    let mut args: Vec<String> = vec!["repo".into(), "list".into()];
    if let Some(owner) = owner {
        args.push(owner.to_string());
    }
    args.extend([
        "--json".to_string(),
        REPO_FIELDS.to_string(),
        "--limit".to_string(),
        limit.to_string(),
    ]);
    GhCommand::new(args).env_remove("GH_REPO")
}

/// The organization lookup, as a command rather than a run.
fn orgs_command() -> GhCommand {
    GhCommand::new(["api", &format!("user/orgs?per_page={ORG_PAGE_SIZE}")]).env_remove("GH_REPO")
}

/// Label used in errors and in [`merge`] for the authenticated user's own repos.
const SELF_OWNER: &str = "your own account";

/// Combine per-owner results into one list, refusing to lose an owner.
///
/// Why: this is the fail-open guard, isolated as a pure function so it is
/// provable without a `gh` install. Returning the successful owners and
/// dropping the failed one would hand the picker a list that is indistinguishable
/// from a complete one — the recipient would silently exclude repositories from
/// the audit (#5487).
/// What: the first `Err` wins and names its owner; otherwise every repository is
/// concatenated, sorted by `name_with_owner`, and de-duplicated (an account can
/// reach the same repository as both owner and org member).
/// Test: `super::discover_tests::one_unreadable_owner_fails_the_whole_discovery`,
/// `super::discover_tests::repositories_reachable_twice_are_listed_once`.
fn merge(
    results: Vec<(String, Result<Vec<DiscoveredRepo>, GhError>)>,
) -> Result<Vec<DiscoveredRepo>, AuditError> {
    let mut all = Vec::new();
    for (owner, result) in results {
        match result {
            Ok(repos) => all.extend(repos),
            Err(source) => {
                return Err(AuditError::Discovery {
                    owner,
                    source: Box::new(source),
                });
            }
        }
    }
    all.sort_by(|a, b| a.name_with_owner.cmp(&b.name_with_owner));
    all.dedup_by(|a, b| a.name_with_owner == b.name_with_owner);
    Ok(all)
}

/// List every repository the recipient's `gh` credential can reach.
///
/// Why: #5487's closure conditions — not limited to a single named org, and
/// covering personal repositories as well as the organizations the token can
/// see. `gh repo list` with no owner answers only the first half, so this also
/// enumerates the account's organizations and lists each one.
/// What: probes the credential, reads `user/orgs`, lists each owner, then
/// [`merge`]s. Sorted by `owner/name`, de-duplicated.
/// Test: `super::discover_tests` for the pure parts;
/// `discovery_against_the_real_gh_credential` (`#[ignore]`) for the whole path.
///
/// # Errors
///
/// [`AuditError::Discovery`] naming the owner whose listing failed — including
/// the credential probe itself, which is attributed to
/// [`SELF_OWNER`]. Every failure is a refusal; none of them is an empty list.
pub async fn discover(limit: u32) -> Result<Vec<DiscoveredRepo>, AuditError> {
    // #5487: probe first, and never keep the value — a token has no business
    // living past the check that it exists.
    credential_probe()
        .nonempty_stdout()
        .await
        .map_err(|source| AuditError::Discovery {
            owner: SELF_OWNER.to_string(),
            source: Box::new(source),
        })?;

    let orgs: Vec<Org> = orgs_command()
        .json()
        .await
        .map_err(|source| AuditError::Discovery {
            owner: "your organizations".to_string(),
            source: Box::new(source),
        })?;

    let mut results: Vec<(String, Result<Vec<DiscoveredRepo>, GhError>)> = Vec::new();
    results.push((
        SELF_OWNER.to_string(),
        list_command(None, limit).json().await,
    ));
    for org in orgs {
        let listed = list_command(Some(&org.login), limit).json().await;
        results.push((org.login, listed));
    }
    merge(results)
}

#[cfg(test)]
mod discover_tests {
    use super::*;

    fn repo(name_with_owner: &str) -> DiscoveredRepo {
        DiscoveredRepo {
            name_with_owner: name_with_owner.to_string(),
            name: name_with_owner
                .split_once('/')
                .map_or(name_with_owner, |(_, n)| n)
                .to_string(),
            is_private: false,
            is_archived: false,
            url: String::new(),
        }
    }

    fn gh_failure() -> GhError {
        GhError::NonZero {
            args: "repo list acme".to_string(),
            status: "exit 1".to_string(),
            stderr: "HTTP 403: Resource not accessible".to_string(),
        }
    }

    #[test]
    fn a_gh_repo_list_payload_parses() {
        let json = r#"[
            {"nameWithOwner":"acme/api","name":"api","isPrivate":true,
             "isArchived":false,"url":"https://github.com/acme/api"}
        ]"#;
        let repos: Vec<DiscoveredRepo> = serde_json::from_str(json).expect("parses");
        assert_eq!(repos[0].name_with_owner, "acme/api");
        assert!(repos[0].is_private);
        assert!(!repos[0].is_archived);
    }

    #[test]
    fn the_credential_probe_asks_gh_for_a_token() {
        assert_eq!(credential_probe().argv_display(), "auth token");
    }

    /// `gh repo list` defaults to 30. Discovery that inherits that default
    /// returns a short list with no signal that it was truncated — the same
    /// fail-open shape as swallowing an error (#5487).
    #[test]
    fn the_listing_always_passes_an_explicit_limit() {
        let argv = list_command(None, DEFAULT_LIMIT).argv_display();
        assert!(argv.contains("--limit 1000"), "{argv}");
        assert!(argv.contains("--json nameWithOwner"), "{argv}");
    }

    #[test]
    fn an_owner_is_placed_before_the_flags() {
        assert_eq!(
            list_command(Some("acme"), 5).argv_display(),
            format!("repo list acme --json {REPO_FIELDS} --limit 5")
        );
    }

    #[test]
    fn the_org_lookup_asks_for_a_full_page() {
        assert_eq!(orgs_command().argv_display(), "api user/orgs?per_page=100");
    }

    /// The fail-open regression: one org the token cannot read must not
    /// silently shorten the list.
    #[test]
    fn one_unreadable_owner_fails_the_whole_discovery() {
        let results = vec![
            (SELF_OWNER.to_string(), Ok(vec![repo("me/personal")])),
            ("acme".to_string(), Err(gh_failure())),
            ("other".to_string(), Ok(vec![repo("other/thing")])),
        ];
        let err = merge(results).expect_err("a failed owner must not be dropped");
        let AuditError::Discovery { owner, .. } = &err else {
            panic!("expected Discovery, got {err:?}");
        };
        assert_eq!(owner, "acme");
        assert!(err.to_string().contains("403"), "{err}");
    }

    #[test]
    fn repositories_reachable_twice_are_listed_once() {
        let results = vec![
            (
                SELF_OWNER.to_string(),
                Ok(vec![repo("acme/api"), repo("me/personal")]),
            ),
            ("acme".to_string(), Ok(vec![repo("acme/api")])),
        ];
        let repos = merge(results).expect("both owners read");
        let names: Vec<&str> = repos.iter().map(|r| r.name_with_owner.as_str()).collect();
        assert_eq!(names, vec!["acme/api", "me/personal"]);
    }

    #[test]
    fn no_owners_at_all_is_an_empty_list_not_an_error() {
        assert!(
            merge(Vec::new())
                .expect("nothing to read is not a failure")
                .is_empty()
        );
    }

    /// The whole path against the recipient's real credential.
    ///
    /// `#[ignore]` because it needs an authenticated `gh` and network —
    /// `cargo test -p trusty-audit -- --include-ignored` runs it. The offline
    /// half of the same guarantee is every test above.
    #[tokio::test]
    #[ignore = "needs an authenticated `gh` and network; run with --include-ignored"]
    async fn discovery_against_the_real_gh_credential() {
        let repos = discover(DEFAULT_LIMIT)
            .await
            .expect("an authenticated gh must list something");
        assert!(
            repos
                .iter()
                .all(|r| r.name_with_owner.contains('/') && !r.name.is_empty()),
            "every discovered repo must carry an owner/name identity"
        );
    }
}
