//! What a registered board contributes to one `tga audit` child.
//!
//! Why: #5857. `crate::registry` could hold a JIRA project or a Linear team
//! since #5822, and nothing downstream read it — `crate::chain::split_targets`
//! forwarded repositories and turned every board into a gap line. So an
//! engagement that registered a board got a non-zero exit and a package saying
//! the board was not covered, for a board tga can collect.
//!
//! What: [`resolve`], which turns the registered boards plus the engagement's
//! [`BoardCredentials`] into the `jira:` / `linear:` sections of the generated
//! tga config, the variables the child needs in its environment, and one gap
//! line per board that still cannot be collected.
//!
//! ## The generated config carries a placeholder, never the secret
//!
//! `crate::run`'s posture is that a secret reaches a child through its
//! environment and never through a file. A board credential does not change
//! that: the generated `state/tga-<stem>.yaml` gets `${TRUSTY_AUDIT_JIRA_TOKEN}`
//! and `crate::run::spawn_tga` puts the real value in the child's environment
//! under that name. tga expands it on the far side —
//! `tga::collect::jira::client::JiraClient::new` expands BOTH `username` and
//! `token` through `expand_credential`, and `tga::collect::linear::client`
//! expands `api_key`. Neither accepts an empty expansion: an unset variable is a
//! `CollectError::Config` naming the field, so this crate adds no second guard
//! of its own.
//!
//! `email` is written literally. It is the Basic-auth username, and
//! `crate::config::JiraCredentials`'s own `Debug` already treats it as
//! non-secret for the same reason — with the password held back it is not on its
//! own a credential.
//!
//! ## Why JIRA writes BOTH fields
//!
//! `JiraClient::new` builds its credential from `(&config.username,
//! &config.token)` and takes the `(Some, Some)` arm only. A config carrying the
//! token alone falls through to `None` and the client runs UNAUTHENTICATED —
//! no error, just an audit that reads nothing. Linear is a different shape: a
//! bare `api_key` with no username, and an absent one is refused outright.
//!
//! Test: `super::boards::boards_tests`.

use serde::Serialize;

use crate::config::BoardCredentials;
use crate::registry::{self, BoardProvider, Target};

/// The variable the JIRA API token reaches the child under.
pub const ENV_JIRA_TOKEN: &str = "TRUSTY_AUDIT_JIRA_TOKEN";

/// The variable the Linear personal API key reaches the child under.
pub const ENV_LINEAR_API_KEY: &str = "TRUSTY_AUDIT_LINEAR_API_KEY";

/// The `${VAR}` reference tga expands, for a variable this crate will set.
fn placeholder(variable: &str) -> String {
    format!("${{{variable}}}")
}

/// tga's `jira:` section, as this client generates it.
///
/// Field names match `tga::core::config::JiraConfig` — `url`, `username`,
/// `token`, `project_key` — because that struct is what parses this document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TgaJira {
    /// Site base URL, from `boards.jira.url`.
    pub url: String,
    /// Basic-auth username: the account email, written literally.
    pub username: String,
    /// Basic-auth password, as a `${…}` reference — never the token itself.
    pub token: String,
    /// The registered project key.
    pub project_key: String,
}

/// tga's `linear:` section, as this client generates it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TgaLinear {
    /// Personal API key, as a `${…}` reference — never the key itself.
    pub api_key: String,
    /// Every registered Linear team key, in registration order.
    pub team_keys: Vec<String>,
}

/// What the registered boards add to one child, and what they cannot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Boards {
    /// The `jira:` section to generate, when a JIRA board is collectable.
    pub jira: Option<TgaJira>,
    /// The `linear:` section to generate, when a Linear board is collectable.
    pub linear: Option<TgaLinear>,
    /// One line per registered board this sweep will not collect, safe to show
    /// the recipient. `crate::chain` carries these into the return package.
    pub gaps: Vec<String>,
}

impl Boards {
    /// Whether any board reaches the child at all.
    pub fn is_empty(&self) -> bool {
        self.jira.is_none() && self.linear.is_none()
    }

    /// The variables the child needs, paired with the real secret values.
    ///
    /// Why: the exposed secret is produced HERE rather than stored on
    /// [`Boards`], so it lives only as long as the `Command` being built —
    /// the same shape as `crate::run::spawn_tga` calling
    /// `SecretKey::expose` inline for the inference credential.
    /// What: one pair per section that [`resolve`] produced, so a variable is
    /// never set for a provider whose section was not written.
    /// Test: `super::boards_tests::the_environment_covers_exactly_the_sections_written`.
    pub fn env<'a>(&self, credentials: &'a BoardCredentials) -> Vec<(&'static str, &'a str)> {
        let mut pairs = Vec::new();
        if self.jira.is_some()
            && let Some(jira) = credentials.jira.as_ref()
        {
            pairs.push((ENV_JIRA_TOKEN, jira.token.expose()));
        }
        if self.linear.is_some()
            && let Some(linear) = credentials.linear.as_ref()
        {
            pairs.push((ENV_LINEAR_API_KEY, linear.api_key.expose()));
        }
        pairs
    }
}

/// Turn the registered boards into config sections, or into stated gaps.
///
/// Why: one function, called by `crate::chain::split_targets` (which needs the
/// gaps) and by `crate::run::sweep` (which needs the sections), so both read the
/// registry through the same rules.
///
/// That alone does NOT make the two agree, and claiming it did was wrong: the
/// same resolver over the same mutable file at two different times can return
/// two different answers, and the chain's two calls were an engagement apart.
/// The guarantee is one resolution per invocation. `crate::chain::audit`
/// resolves here once and hands the result to `crate::run::sweep_with_boards`;
/// `taudit run` resolves here once inside `crate::run::sweep`. Nothing calls it
/// twice for one run.
///
/// # Postconditions
/// Every element of `targets` that is a [`Target::Board`] either contributes to
/// [`Boards::jira`] / [`Boards::linear`] or appears in [`Boards::gaps`], never
/// both and never neither. No returned value contains a secret: the credential
/// fields are `${VAR}` references.
///
/// What: a board whose provider has no entry in the engagement config is a gap
/// naming the field to set — that is the same fact
/// [`crate::error::AuditError::BoardCredentialMissing`] states at registration
/// time, reached here because a config can lose a section after a board was
/// registered. Linear teams accumulate into one section, because tga's
/// `linear.team_keys` is a list. A SECOND JIRA project is a gap: tga's
/// `jira.project_key` is a single value, so the extra board would otherwise be
/// silently dropped. A Linear team key is UPPERCASED on the way into
/// `team_keys`, because tga's collector matches it case-insensitively and its
/// classifier matches it exactly — so a lowercase key collected issues and
/// classified none, and uppercasing is what makes one stored key mean one thing
/// to both. A key that is not a team key [`registry::is_linear_team_key`]
/// recognises even uppercased is a gap: it would otherwise reach `team_keys`,
/// match no issue, and be reported as a covered board that happened to be quiet
/// (#5982).
/// Test: `super::boards_tests`.
pub fn resolve(targets: &[Target], credentials: &BoardCredentials) -> Boards {
    let mut resolved = Boards::default();
    let mut linear_teams = Vec::new();
    for target in targets {
        let Target::Board { provider, key } = target else {
            continue;
        };
        match provider {
            BoardProvider::Jira => match credentials.jira.as_ref() {
                None => resolved.gaps.push(unconfigured(target, *provider)),
                Some(_) if resolved.jira.is_some() => resolved.gaps.push(second_jira(target)),
                Some(creds) => {
                    resolved.jira = Some(TgaJira {
                        url: creds.url.clone(),
                        username: creds.email.clone(),
                        token: placeholder(ENV_JIRA_TOKEN),
                        project_key: key.clone(),
                    });
                }
            },
            BoardProvider::Linear => match credentials.linear.as_ref() {
                None => resolved.gaps.push(unconfigured(target, *provider)),
                // #5982: uppercase first, gap only on what uppercasing cannot
                // save. tga reads a stored key twice and disagrees with itself:
                // `collect::linear::client::fetch_referenced_issues` compares
                // `team_keys` with `eq_ignore_ascii_case`, so `eng` collected
                // `ENG-1234`, while `classify::sources::linear::matches_team_key`
                // compares exactly, so the same issue classified as nothing.
                // Uppercasing keeps the collection and adds the classification;
                // gapping a lowercase key would have taken the collection away.
                Some(_) => {
                    let key = key.to_ascii_uppercase();
                    if registry::is_linear_team_key(&key) {
                        linear_teams.push(key);
                    } else {
                        resolved.gaps.push(uncollectable_linear(target));
                    }
                }
            },
        }
    }
    if !linear_teams.is_empty() {
        resolved.linear = Some(TgaLinear {
            api_key: placeholder(ENV_LINEAR_API_KEY),
            team_keys: linear_teams,
        });
    }
    resolved
}

fn unconfigured(target: &Target, provider: BoardProvider) -> String {
    format!(
        "{target} was not audited — this engagement's config carries no `{}` credential (#5857)",
        provider.config_field()
    )
}

/// The gap for a Linear board stored as something no issue can carry.
///
/// #5982 reached this state through registration, which accepted a team id and
/// wrote it down; that half is fixed at the source now. This stays because the
/// file outlives the fix — a target registered by id before it is still on disk,
/// and a run that skipped it silently is the failure this line replaces on the
/// paths that state gaps: `taudit audit`, and `taudit run` via
/// [`crate::run::RunReport::board_gaps`].
///
/// The line names the SHAPE rather than guessing what the stored key is. It is
/// reached by a team id, by an over-long key, and by anything carrying a
/// character a Linear identifier cannot — calling every one of them a team id
/// was wrong. It also names BOTH halves of the remedy: registering the team key
/// leaves the old entry in place, and a registry still holding it keeps
/// answering [`crate::session::EXIT_INCOMPLETE`] for a board that is now
/// audited.
fn uncollectable_linear(target: &Target) -> String {
    format!(
        "{target} was not audited — `tga audit` matches Linear issues by team key (the `ENG` in \
         `ENG-1234`), which is one to ten characters of A-Z and 0-9 starting with a letter, and \
         that key is not; re-register it as `linear:<TEAM-KEY>` AND drop this entry with `taudit \
         remove {target}`, or every later run keeps reporting it (#5982)"
    )
}

fn second_jira(target: &Target) -> String {
    format!(
        "{target} was not audited — `tga audit` takes one JIRA project per run, and another was \
         registered first (#5857)"
    )
}

#[cfg(test)]
mod boards_tests {
    use super::*;
    use crate::config::{JiraCredentials, LinearCredentials, SecretKey};
    use crate::registry::{self, TargetKind};

    const JIRA_TOKEN: &str = "jira-token-do-not-write-me-down";
    const LINEAR_KEY: &str = "lin_api_do-not-write-me-down";

    /// A Linear team id, in the shape the API returns one. See #5982.
    const TEAM_ID: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

    fn board(spec: &str) -> Target {
        registry::parse(Some(TargetKind::Board), spec).expect("parses")
    }

    fn jira_creds() -> JiraCredentials {
        JiraCredentials {
            url: "https://acme.atlassian.net".to_owned(),
            email: "auditor@acme.example".to_owned(),
            token: SecretKey::new(JIRA_TOKEN),
        }
    }

    fn both() -> BoardCredentials {
        BoardCredentials {
            jira: Some(jira_creds()),
            linear: Some(LinearCredentials {
                api_key: SecretKey::new(LINEAR_KEY),
            }),
        }
    }

    /// The trap #5857 names: `JiraClient::new` takes its `(Some, Some)` arm
    /// only, so a section carrying the token alone yields an UNAUTHENTICATED
    /// client and an audit that silently reads nothing. Both fields, or the
    /// wiring is worse than the gap it replaced.
    #[test]
    fn a_jira_board_writes_both_the_username_and_the_token() {
        let resolved = resolve(&[board("jira:ACME")], &both());
        let jira = resolved
            .jira
            .expect("a configured JIRA board is collectable");
        assert_eq!(jira.username, "auditor@acme.example");
        assert_eq!(jira.token, "${TRUSTY_AUDIT_JIRA_TOKEN}");
        assert_eq!(jira.project_key, "ACME");
        assert_eq!(jira.url, "https://acme.atlassian.net");
        assert!(resolved.gaps.is_empty(), "{:?}", resolved.gaps);
    }

    /// Linear is a bare key with no username, so modelling it on JIRA's pair
    /// would invent a field tga's `LinearConfig` does not have.
    #[test]
    fn a_linear_board_is_a_bare_key_and_a_team_list() {
        let resolved = resolve(&[board("linear:ENG")], &both());
        let linear = resolved.linear.expect("a configured Linear board");
        assert_eq!(linear.api_key, "${TRUSTY_AUDIT_LINEAR_API_KEY}");
        assert_eq!(linear.team_keys, vec!["ENG".to_owned()]);
        assert!(resolved.jira.is_none(), "{:?}", resolved.jira);
    }

    /// `linear.team_keys` is a list at tga, so a second team is coverage rather
    /// than a gap.
    #[test]
    fn every_registered_linear_team_reaches_the_one_section() {
        let resolved = resolve(&[board("linear:ENG"), board("linear:FE")], &both());
        let linear = resolved.linear.expect("a configured Linear board");
        assert_eq!(linear.team_keys, vec!["ENG".to_owned(), "FE".to_owned()]);
        assert!(resolved.gaps.is_empty(), "{:?}", resolved.gaps);
    }

    /// A team id is what #5982 names: `crate::validate` accepted one, and tga
    /// compares `team_keys` against the text before the hyphen in `ENG-1234`,
    /// where a UUID never appears. The id used to reach `team_keys` and the run
    /// reported a covered board that contributed nothing — indistinguishable
    /// from a team with no issues.
    #[test]
    fn a_registered_team_id_is_a_gap_rather_than_a_team_that_collects_nothing() {
        let resolved = resolve(&[board(&format!("linear:{TEAM_ID}"))], &both());
        assert!(resolved.linear.is_none(), "{:?}", resolved.linear);
        assert_eq!(resolved.gaps.len(), 1, "{:?}", resolved.gaps);
        assert!(resolved.gaps[0].contains(TEAM_ID), "{:?}", resolved.gaps);
        assert!(resolved.gaps[0].contains("#5982"), "{:?}", resolved.gaps);
    }

    /// The lowercase key #5982's fix must not cost anything. tga's collector
    /// (`fetch_referenced_issues`) compares `team_keys` with
    /// `eq_ignore_ascii_case`, so `linear:eng` already collected `ENG-1234`;
    /// only its classifier (`matches_team_key`) compared exactly and matched
    /// nothing. Gapping the key would have turned partial collection into none.
    /// Uppercasing keeps the collection and adds the classification.
    #[test]
    fn a_lowercase_legacy_key_collects_uppercased_rather_than_gapping() {
        let resolved = resolve(&[board("linear:eng")], &both());
        let linear = resolved
            .linear
            .expect("a lowercase key still reaches the child");
        assert_eq!(linear.team_keys, vec!["ENG".to_owned()]);
        assert!(resolved.gaps.is_empty(), "{:?}", resolved.gaps);
    }

    /// The remedy the gap line states has two halves, and stating only the
    /// first leaves the registry answering `EXIT_INCOMPLETE` forever for a board
    /// the operator has since re-registered correctly. The line also describes
    /// the key SHAPE rather than asserting the stored key is a team id — it is
    /// reached by an over-long key too.
    #[test]
    fn the_gap_line_states_the_shape_and_both_halves_of_the_remedy() {
        let resolved = resolve(&[board("linear:ABCDEFGHIJK")], &both());
        assert!(resolved.linear.is_none(), "{:?}", resolved.linear);
        let gap = &resolved.gaps[0];
        assert!(!gap.contains("that is a team id"), "{gap}");
        assert!(
            gap.contains("re-register it as `linear:<TEAM-KEY>`"),
            "{gap}"
        );
        assert!(gap.contains("`taudit remove linear:ABCDEFGHIJK`"), "{gap}");
    }

    /// The gap costs the teams around it nothing: a stale id alongside a real
    /// team key leaves that team collecting exactly as it did.
    #[test]
    fn a_team_id_does_not_displace_the_team_keys_that_do_collect() {
        let resolved = resolve(
            &[board("linear:ENG"), board(&format!("linear:{TEAM_ID}"))],
            &both(),
        );
        let linear = resolved.linear.clone().expect("ENG still collects");
        assert_eq!(linear.team_keys, vec!["ENG".to_owned()]);
        assert_eq!(resolved.gaps.len(), 1, "{:?}", resolved.gaps);
    }

    /// `jira.project_key` is one value, so the second project has nowhere to go.
    /// Stating it beats dropping it.
    #[test]
    fn a_second_jira_project_is_a_gap_rather_than_a_silent_drop() {
        let resolved = resolve(&[board("jira:ACME"), board("jira:OPS")], &both());
        assert_eq!(
            resolved.jira.expect("the first project").project_key,
            "ACME"
        );
        assert_eq!(resolved.gaps.len(), 1, "{:?}", resolved.gaps);
        assert!(resolved.gaps[0].contains("jira:OPS"), "{:?}", resolved.gaps);
    }

    /// A board registered against a config that says nothing about its provider
    /// is the one board case that is still a gap — and the line names the field
    /// to set, as `BoardCredentialMissing` does at registration time.
    #[test]
    fn a_board_with_no_configured_credential_is_a_gap_naming_the_field() {
        let none = BoardCredentials::default();
        let resolved = resolve(&[board("jira:ACME"), board("linear:ENG")], &none);
        assert!(resolved.is_empty(), "{resolved:?}");
        assert_eq!(resolved.gaps.len(), 2, "{:?}", resolved.gaps);
        assert!(
            resolved.gaps[0].contains("boards.jira"),
            "{:?}",
            resolved.gaps
        );
        assert!(
            resolved.gaps[1].contains("boards.linear"),
            "{:?}",
            resolved.gaps
        );
    }

    /// Nothing this function returns may carry a secret — the config document
    /// it feeds is written to disk.
    #[test]
    fn no_resolved_section_carries_the_secret_itself() {
        let resolved = resolve(&[board("jira:ACME"), board("linear:ENG")], &both());
        let rendered = format!("{resolved:?}");
        assert!(!rendered.contains(JIRA_TOKEN), "{rendered}");
        assert!(!rendered.contains(LINEAR_KEY), "{rendered}");
    }

    /// A variable is set only for a section that was written, so a provider
    /// that gapped never exports a credential the child has no config to use.
    #[test]
    fn the_environment_covers_exactly_the_sections_written() {
        let credentials = both();
        let jira_only = resolve(&[board("jira:ACME")], &credentials);
        assert_eq!(
            jira_only.env(&credentials),
            vec![(ENV_JIRA_TOKEN, JIRA_TOKEN)]
        );

        let neither = resolve(&[], &credentials);
        assert!(neither.env(&credentials).is_empty());

        let both_boards = resolve(&[board("jira:ACME"), board("linear:ENG")], &credentials);
        assert_eq!(
            both_boards.env(&credentials),
            vec![
                (ENV_JIRA_TOKEN, JIRA_TOKEN),
                (ENV_LINEAR_API_KEY, LINEAR_KEY)
            ]
        );
    }

    /// A repository in the same registry is not this module's business.
    #[test]
    fn a_repository_target_contributes_nothing() {
        let repo = registry::parse(Some(TargetKind::Repo), "acme/api").expect("parses");
        assert_eq!(resolve(&[repo], &both()), Boards::default());
    }
}
