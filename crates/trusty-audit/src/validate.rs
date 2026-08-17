//! Proving a target can be read, before it is registered.
//!
//! Why: #5822. `taudit add` writes into a file the sweep later acts on, and a
//! target recorded without ever being reached is a failure deferred to the
//! middle of an hour-long unattended run — where DOC-67 §9's continue-on-failure
//! model turns it into a gap in a report someone has already been told is
//! coming. Checking at registration costs one request and moves that failure to
//! the moment the operator can still do something about it.
//!
//! What: one function per provider, each using the credential the SWEEP will
//! use, so a pass here means the same access later:
//!
//! | Target | Credential | Check |
//! |---|---|---|
//! | repository | the recipient's `gh` login | `gh auth token`, then `gh repo view` |
//! | `jira:KEY` | `boards.jira` in the engagement config | `GET /rest/api/3/project/KEY` |
//! | `linear:KEY` | `boards.linear` | a GraphQL read of the teams that key can see |
//!
//! Nothing here is a collector. tga owns collection from both boards
//! (`tga::collect::jira`, `tga::collect::linear`); this module borrows their
//! auth and endpoint conventions — HTTP Basic with `email` / API token for JIRA,
//! a bare `Authorization` header with no `Bearer` prefix for Linear — and
//! reads exactly enough to answer "can this credential see that".
//!
//! ## The credential never reaches a message
//!
//! Every refusal is built from a status code, a transport classification, or a
//! provider-authored message run through
//! [`trusty_common::credentials::scrub_secrets`]. `reqwest`'s own `Display` is
//! deliberately never used: it renders the request URL, and an operator who put
//! credentials in `boards.jira.url` would have them in the error text.
//! Test: `super::validate_tests::a_transport_failure_reason_names_no_url`,
//! `super::validate_tests::a_provider_message_is_scrubbed_of_the_credential`.
//!
//! Test: `super::validate_tests`, plus the `#[ignore]`d live probes there.

use std::time::Duration;

use serde::Deserialize;
use trusty_common::credentials::scrub_secrets;
use trusty_common::gh::GhCommand;

use crate::config::{EngagementConfig, JiraCredentials, LinearCredentials};
use crate::discover;
use crate::error::AuditError;
use crate::registry::{BoardProvider, Target};

/// Ceiling on one validation request.
///
/// A registration is interactive — the operator is at the keyboard waiting —
/// so an unreachable site has to come back as a refusal rather than a hang.
/// `trusty_installer::download::http_client` builds a client with no timeout
/// because it also serves multi-megabyte downloads, so the bound is applied per
/// request here instead of by building a second client.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Linear's single GraphQL endpoint, as `tga::collect::linear` uses it.
const LINEAR_GRAPHQL_URL: &str = "https://api.linear.app/graphql";

/// Teams to ask Linear for in one page. A workspace past this is not listed.
const LINEAR_TEAM_PAGE: u32 = 250;

/// Characters of provider-authored text carried into an operator-visible
/// message, matching `tga::collect::linear`'s own cap.
const MAX_REASON_CHARS: usize = 300;

/// Which `gh` invocations the repository check runs.
///
/// Why: #5822 — `validate_linear` takes its `endpoint` for exactly this reason,
/// and the repository arm had no equivalent. Its only failure coverage was an
/// `#[ignore]`d test needing network and an authenticated `gh`, so a default
/// `cargo test -p trusty-audit` proved nothing about a refused repository. Two
/// commands carried as data let a test supply a pair that cannot succeed,
/// offline, without a second `Command::new("gh")` anywhere (CLAUDE.md's
/// common-entry-point rule).
/// What: function pointers rather than built commands, because the repository
/// read needs the `owner/name` only the call site has. [`RepoProbe::real`] is
/// what every front end gets.
/// Test: `super::validate_tests::a_gh_that_cannot_answer_refuses_the_repository`.
#[derive(Debug, Clone, Copy)]
pub struct RepoProbe {
    /// Builds the credential probe.
    credential: fn() -> GhCommand,
    /// Builds the repository read for one `owner/name`.
    view: fn(&str) -> GhCommand,
}

impl RepoProbe {
    /// The real pair: `crate::discover`'s one credential probe, then
    /// [`view_command`].
    pub fn real() -> Self {
        Self {
            credential: discover::credential_probe,
            view: view_command,
        }
    }

    /// A pair that cannot answer, for proving the refusal path offline.
    ///
    /// Why: `gh` has no [`NO_SUCH_SUBCOMMAND`], so an installed `gh` exits
    /// non-zero and an absent one is `GhError::NotInstalled`. Both are
    /// refusals, reached with no network and no authenticated account — which
    /// is what makes the repository arm's fail-closed contract provable in a
    /// default `cargo test -p trusty-audit` (#5822).
    /// Test: `super::validate_tests::a_gh_that_cannot_answer_refuses_the_repository`,
    /// `crate::session::session_tests::a_refused_repository_registration_writes_nothing`.
    #[cfg(test)]
    pub(crate) fn unusable() -> Self {
        Self {
            credential: || GhCommand::new([NO_SUCH_SUBCOMMAND]),
            view: |_| GhCommand::new([NO_SUCH_SUBCOMMAND]),
        }
    }
}

/// A `gh` subcommand that does not exist. See [`RepoProbe::unusable`].
#[cfg(test)]
const NO_SUCH_SUBCOMMAND: &str = "no-such-subcommand-5822";

/// Prove `target` can be read with the credential the sweep will use.
///
/// Why: the gate `crate::registry` runs before it persists anything. Returning
/// `Ok(())` rather than data is deliberate — this answers one question, and a
/// caller that also wanted the project's name would be tempted to make this a
/// second collector.
/// What: dispatches on the target. A board whose provider has no credential in
/// the engagement config is refused HERE rather than at the request, so the
/// message names the config field instead of an HTTP 401. `gh` carries the
/// repository arm's invocations — see [`RepoProbe`].
/// Test: `super::validate_tests::a_board_with_no_credential_names_the_field`,
/// and the `#[ignore]`d live probes.
///
/// # Errors
///
/// [`AuditError::RepoUnreachable`] when `gh` cannot read the repository,
/// [`AuditError::BoardCredentialMissing`] when the engagement config carries no
/// credential for the provider, and [`AuditError::BoardUnreachable`] when the
/// credential exists and cannot read the board.
pub async fn validate(
    target: &Target,
    config: Option<&EngagementConfig>,
    gh: RepoProbe,
) -> Result<(), AuditError> {
    match target {
        Target::Repo { name_with_owner } => validate_repo(name_with_owner, gh).await,
        Target::Board { provider, key } => match provider {
            BoardProvider::Jira => {
                let creds = config
                    .and_then(|c| c.boards.jira.as_ref())
                    .filter(|c| !c.token.is_empty() && !c.url.trim().is_empty())
                    .ok_or_else(|| missing(*provider, key))?;
                validate_jira(creds, key).await
            }
            BoardProvider::Linear => {
                let creds = config
                    .and_then(|c| c.boards.linear.as_ref())
                    .filter(|c| !c.api_key.is_empty())
                    .ok_or_else(|| missing(*provider, key))?;
                validate_linear(creds, key, LINEAR_GRAPHQL_URL).await
            }
        },
    }
}

/// The refusal for a board whose provider was never configured.
fn missing(provider: BoardProvider, key: &str) -> AuditError {
    AuditError::BoardCredentialMissing {
        provider: provider.as_str(),
        key: key.to_owned(),
        field: provider.config_field(),
    }
}

/// The `gh` read that proves a repository is reachable.
///
/// `--json nameWithOwner` rather than a bare `gh repo view`: the plain form
/// prints a rendered README page, and asking for a field means the reply is
/// parsed rather than merely non-empty.
fn view_command(name_with_owner: &str) -> GhCommand {
    GhCommand::new(["repo", "view", name_with_owner, "--json", "nameWithOwner"])
        .env_remove("GH_REPO")
}

/// The subset of `gh repo view --json` this module reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ViewedRepo {
    #[allow(dead_code, reason = "parsing it is the check; the value is not used")]
    name_with_owner: String,
}

/// Prove the recipient's `gh` credential can read this repository.
///
/// The probe comes first for the reason `crate::discover` states: a whitespace
/// `GH_TOKEN` makes `gh auth token` exit ZERO with blank stdout, and without
/// the probe the failure would surface as a confusing "no such repository".
async fn validate_repo(name_with_owner: &str, gh: RepoProbe) -> Result<(), AuditError> {
    let refuse = |source| AuditError::RepoUnreachable {
        name_with_owner: name_with_owner.to_owned(),
        source: Box::new(source),
    };
    // #5822: both invocations come from the probe, so a test can supply a pair
    // that cannot answer and prove the refusal offline.
    (gh.credential)().nonempty_stdout().await.map_err(refuse)?;
    (gh.view)(name_with_owner)
        .json::<ViewedRepo>()
        .await
        .map(|_| ())
        .map_err(refuse)
}

/// Prove the configured JIRA credential can read this project.
///
/// `GET /rest/api/3/project/{key}` is the smallest read that answers both
/// halves at once — a bad credential is a 401 and an unreadable project is a
/// 403 or 404 — and it is the same REST base and Basic-auth pairing
/// `tga::collect::jira` uses.
async fn validate_jira(creds: &JiraCredentials, key: &str) -> Result<(), AuditError> {
    let url = format!(
        "{}/rest/api/3/project/{key}",
        creds.url.trim().trim_end_matches('/')
    );
    let response = trusty_installer::download::http_client()
        .get(&url)
        .basic_auth(&creds.email, Some(creds.token.expose()))
        .header("Accept", "application/json")
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| board_refusal(BoardProvider::Jira, key, transport_reason(&e)))?;

    if response.status().is_success() {
        return Ok(());
    }
    Err(board_refusal(
        BoardProvider::Jira,
        key,
        status_reason("JIRA", "project", response.status().as_u16()),
    ))
}

/// A Linear GraphQL reply: `data` when it worked, `errors` when it did not.
#[derive(Debug, Deserialize)]
struct GraphQlReply {
    #[serde(default)]
    data: Option<TeamsData>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    #[serde(default)]
    message: String,
}

#[derive(Debug, Deserialize)]
struct TeamsData {
    teams: TeamNodes,
}

#[derive(Debug, Deserialize)]
struct TeamNodes {
    #[serde(default)]
    nodes: Vec<Team>,
}

#[derive(Debug, Deserialize)]
struct Team {
    #[serde(default)]
    id: String,
    #[serde(default)]
    key: String,
}

/// Prove the configured Linear credential can read this team.
///
/// One query lists the teams the key can see, and the match is made here rather
/// than by a server-side filter: that answers "is the credential usable" and "is
/// this team visible to it" in a single round trip, and a workspace whose team
/// list is empty is then distinguishable from one where the key simply does not
/// match. `endpoint` is a parameter so the live probe below can point elsewhere.
async fn validate_linear(
    creds: &LinearCredentials,
    key: &str,
    endpoint: &str,
) -> Result<(), AuditError> {
    let query = format!("query {{ teams(first: {LINEAR_TEAM_PAGE}) {{ nodes {{ id key }} }} }}");
    let response = trusty_installer::download::http_client()
        .post(endpoint)
        // No `Bearer` prefix — Linear's own convention, as `tga` documents.
        .header("Authorization", creds.api_key.expose())
        .header("Content-Type", "application/json")
        .timeout(REQUEST_TIMEOUT)
        .body(
            serde_json::json!({ "query": query })
                .to_string()
                .into_bytes(),
        )
        .send()
        .await
        .map_err(|e| board_refusal(BoardProvider::Linear, key, transport_reason(&e)))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| board_refusal(BoardProvider::Linear, key, transport_reason(&e)))?;
    let reply: GraphQlReply = serde_json::from_str(&body).map_err(|_| {
        board_refusal(
            BoardProvider::Linear,
            key,
            status_reason("Linear", "team", status.as_u16()),
        )
    })?;

    if !reply.errors.is_empty() {
        return Err(board_refusal(
            BoardProvider::Linear,
            key,
            graphql_reason(&reply.errors, creds.api_key.expose()),
        ));
    }
    let teams = reply.data.map(|d| d.teams.nodes).unwrap_or_default();
    if teams
        .iter()
        .any(|t| t.key.eq_ignore_ascii_case(key) || t.id.eq_ignore_ascii_case(key))
    {
        return Ok(());
    }
    Err(board_refusal(
        BoardProvider::Linear,
        key,
        format!(
            "the credential reaches {} team(s), and none of them is that one",
            teams.len()
        ),
    ))
}

fn board_refusal(provider: BoardProvider, key: &str, reason: String) -> AuditError {
    AuditError::BoardUnreachable {
        provider: provider.as_str(),
        key: key.to_owned(),
        reason,
    }
}

/// Why a non-success status is a refusal, in words that name no request detail.
fn status_reason(provider: &str, noun: &str, status: u16) -> String {
    match status {
        401 => format!("the configured {provider} credential was rejected (HTTP 401)"),
        403 => format!("the configured {provider} credential cannot see that {noun} (HTTP 403)"),
        404 => format!("no such {provider} {noun} (HTTP 404)"),
        other => format!("{provider} answered HTTP {other}"),
    }
}

/// Classify a transport failure without quoting the request.
///
/// `reqwest::Error`'s `Display` renders the URL it was given, so a credential
/// embedded in `boards.jira.url` would ride into the message. This names the
/// failure class instead, which is what an operator acts on anyway.
fn transport_reason(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        "the request timed out".to_owned()
    } else if err.is_connect() {
        "the site could not be reached".to_owned()
    } else if err.is_decode() {
        "the reply was not the JSON expected".to_owned()
    } else {
        "the request did not complete".to_owned()
    }
}

/// Fold provider-authored GraphQL errors into one scrubbed, bounded line.
fn graphql_reason(errors: &[GraphQlError], secret: &str) -> String {
    let joined = errors
        .iter()
        .map(|e| e.message.trim())
        .filter(|m| !m.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    let joined = if joined.is_empty() {
        "the request was refused and no reason was given".to_owned()
    } else {
        joined
    };
    truncate(&scrub_secrets(&joined, &[secret]), MAX_REASON_CHARS)
}

/// Cut to at most `max` characters, on a character boundary.
fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_owned(),
        Some((cut, _)) => format!("{}…", &text[..cut]),
    }
}

#[cfg(test)]
mod validate_tests {
    use super::*;
    use crate::config::SecretKey;

    const KEY: &str = "lin_api_a-real-looking-secret";

    fn linear_creds() -> LinearCredentials {
        LinearCredentials {
            api_key: SecretKey::new(KEY),
        }
    }

    fn config_with(text: &str) -> EngagementConfig {
        EngagementConfig::from_toml(text, std::path::Path::new("engagement.toml"))
            .expect("the fixture parses")
    }

    const BASE_CONFIG: &str = r#"
openrouter_key = "sk-or-v1-x"
instructions = "assess"

[tools]
tga = "2.9.4"
trusty-search = "0.47.0"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"
"#;

    fn board(provider: BoardProvider, key: &str) -> Target {
        Target::Board {
            provider,
            key: key.to_owned(),
        }
    }

    /// The whole point of failing here rather than at the request: the operator
    /// is told which config field to set, not shown an HTTP 401.
    #[tokio::test]
    async fn a_board_with_no_credential_names_the_field() {
        let config = config_with(BASE_CONFIG);
        for (provider, field) in [
            (BoardProvider::Jira, "boards.jira"),
            (BoardProvider::Linear, "boards.linear"),
        ] {
            let err = validate(&board(provider, "ACME"), Some(&config), RepoProbe::real())
                .await
                .expect_err("an unconfigured provider cannot be checked");
            let AuditError::BoardCredentialMissing { field: named, .. } = &err else {
                panic!("expected BoardCredentialMissing, got {err:?}");
            };
            assert_eq!(*named, field);
            assert!(err.to_string().contains(field), "{err}");
        }
    }

    /// A config with no config at all behaves the same way — the guided flow
    /// runs against working directories that have none.
    #[tokio::test]
    async fn a_board_with_no_config_at_all_names_the_field() {
        let err = validate(
            &board(BoardProvider::Linear, "ENG"),
            None,
            RepoProbe::real(),
        )
        .await
        .expect_err("no config, no check");
        assert!(matches!(err, AuditError::BoardCredentialMissing { .. }));
    }

    /// A field that is present and blank is not a configured credential — it
    /// would otherwise become an HTTP 401 the operator has to decode.
    #[tokio::test]
    async fn a_blank_credential_reads_as_absent() {
        let config = config_with(&format!(
            "{BASE_CONFIG}\n[boards.linear]\napi_key = \"  \"\n"
        ));
        let err = validate(
            &board(BoardProvider::Linear, "ENG"),
            Some(&config),
            RepoProbe::real(),
        )
        .await
        .expect_err("a blank key is not a credential");
        assert!(
            matches!(err, AuditError::BoardCredentialMissing { .. }),
            "{err:?}"
        );

        let config = config_with(&format!(
            "{BASE_CONFIG}\n[boards.jira]\nurl = \"\"\nemail = \"a@b.c\"\ntoken = \"t\"\n"
        ));
        let err = validate(
            &board(BoardProvider::Jira, "ACME"),
            Some(&config),
            RepoProbe::real(),
        )
        .await
        .expect_err("a blank site URL is not a credential");
        assert!(
            matches!(err, AuditError::BoardCredentialMissing { .. }),
            "{err:?}"
        );
    }

    /// The credential must not reach an error string, asserted directly rather
    /// than argued from where the value is used.
    #[test]
    fn a_provider_message_is_scrubbed_of_the_credential() {
        let errors = vec![GraphQlError {
            message: format!("Authentication failed for token {KEY}"),
        }];
        let reason = graphql_reason(&errors, KEY);
        assert!(!reason.contains(KEY), "the key leaked: {reason}");
        assert!(reason.contains("Authentication failed"), "{reason}");

        let err = board_refusal(BoardProvider::Linear, "ENG", reason);
        assert!(!err.to_string().contains(KEY), "{err}");
    }

    #[test]
    fn an_empty_graphql_error_list_still_says_something() {
        let reason = graphql_reason(
            &[GraphQlError {
                message: "  ".into(),
            }],
            KEY,
        );
        assert!(reason.contains("no reason was given"), "{reason}");
    }

    #[test]
    fn a_long_provider_message_is_bounded() {
        let errors = vec![GraphQlError {
            message: "x".repeat(MAX_REASON_CHARS * 2),
        }];
        let reason = graphql_reason(&errors, KEY);
        assert!(reason.chars().count() <= MAX_REASON_CHARS + 1, "{reason}");
    }

    /// A URL is never quoted, so a credential an operator embedded in
    /// `boards.jira.url` cannot ride into the message.
    #[tokio::test]
    async fn a_transport_failure_reason_names_no_url() {
        let config = config_with(&format!(
            "{BASE_CONFIG}\n[boards.jira]\nurl = \"http://user:hunter2@127.0.0.1:1\"\n\
             email = \"a@b.c\"\ntoken = \"jira-token-secret\"\n"
        ));
        let err = validate(
            &board(BoardProvider::Jira, "ACME"),
            Some(&config),
            RepoProbe::real(),
        )
        .await
        .expect_err("port 1 on loopback refuses the connection");
        let rendered = err.to_string();
        assert!(
            matches!(err, AuditError::BoardUnreachable { .. }),
            "{err:?}"
        );
        assert!(!rendered.contains("hunter2"), "the URL leaked: {rendered}");
        assert!(
            !rendered.contains("jira-token-secret"),
            "the token leaked: {rendered}"
        );
        assert!(!rendered.contains("127.0.0.1"), "{rendered}");
    }

    #[test]
    fn status_reasons_distinguish_a_bad_credential_from_a_missing_board() {
        assert!(status_reason("JIRA", "project", 401).contains("rejected"));
        assert!(status_reason("JIRA", "project", 403).contains("cannot see"));
        assert!(status_reason("JIRA", "project", 404).contains("no such"));
        assert!(status_reason("Linear", "team", 500).contains("HTTP 500"));
    }

    /// The repository arm's fail-closed contract, offline (#5822). Until the
    /// probe became a parameter, this path's only coverage was the `#[ignore]`d
    /// live test below, so a default `cargo test -p trusty-audit` never
    /// exercised a refused repository at all.
    #[tokio::test]
    async fn a_gh_that_cannot_answer_refuses_the_repository() {
        let target = Target::Repo {
            name_with_owner: "acme/api".to_owned(),
        };
        let err = validate(&target, None, RepoProbe::unusable())
            .await
            .expect_err("a gh that cannot answer must not register a repository");
        assert!(matches!(err, AuditError::RepoUnreachable { .. }), "{err:?}");
        assert!(err.to_string().contains("acme/api"), "{err}");
    }

    #[test]
    fn the_repository_read_asks_for_a_field_rather_than_a_page() {
        let argv = view_command("acme/api").argv_display();
        assert_eq!(argv, "repo view acme/api --json nameWithOwner");
    }

    /// The whole repository path against the recipient's real credential and a
    /// real `gh` — the coverage the offline test above adds to rather than
    /// replaces.
    ///
    /// `#[ignore]` because it needs an authenticated `gh` and network —
    /// `cargo test -p trusty-audit -- --include-ignored` runs it.
    #[tokio::test]
    #[ignore = "needs an authenticated `gh` and network; run with --include-ignored"]
    async fn a_repository_that_does_not_exist_is_refused() {
        let err = validate_repo("bobmatnyc/no-such-repository-5822", RepoProbe::real())
            .await
            .expect_err("a repository that does not exist must not register");
        assert!(matches!(err, AuditError::RepoUnreachable { .. }), "{err:?}");
    }

    /// A key Linear will reject, against the real endpoint: the refusal must
    /// carry the provider's reason and none of the key.
    ///
    /// `#[ignore]` because it reaches api.linear.app.
    #[tokio::test]
    #[ignore = "reaches api.linear.app; run with --include-ignored"]
    async fn a_rejected_linear_key_never_appears_in_the_refusal() {
        let err = validate_linear(&linear_creds(), "ENG", LINEAR_GRAPHQL_URL)
            .await
            .expect_err("a key that was never issued cannot read a team");
        let rendered = err.to_string();
        assert!(
            matches!(err, AuditError::BoardUnreachable { .. }),
            "{err:?}"
        );
        assert!(!rendered.contains(KEY), "the key leaked: {rendered}");
    }
}
