//! Generic org and user channel routing (#10417, epic #10416 S1).
//!
//! Why: owner ruling (Bob 03:1xZ): "I should be able to connect either to
//! either." An org binds to a channel kind in config, never in code, and a
//! message to a user goes only through that user's own org channel ("Duetto
//! never needs to talk to owner@example.com").
//! What: [`OrgsConfig`] parses the `orgs:` / `users:` / `owner:` YAML
//! document; [`OrgsConfig::route`] resolves a user to the one [`OrgRoute`]
//! that may carry a message to them, and refuses a send through any other
//! org ([`RouteError::CrossOrg`]); [`OrgsConfig::approval_target`] names the
//! owner's Slack DM for every org, with the dashboard as fallback.
//! Credentials are `secret://` names only; this module never holds a value.
//! Test: `orgs_tests.rs`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The scheme every credential ref must carry; a value never appears here.
pub const SECRET_SCHEME: &str = "secret://";

/// The channel kind an org binds to. Config chooses; no org is fixed to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    /// Slack: a user address is a member id (`U…` or `W…`).
    Slack,
    /// Google Chat: a user address is an email.
    Gchat,
}

/// One org's channel binding and its credential refs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgEntry {
    /// The channel kind this org uses.
    pub kind: ChannelKind,
    /// Slack workspace (team) id, for a Slack org.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Google Cloud project, for a Google Chat org.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Google service account name, for a Google Chat org.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_account: Option<String>,
    /// Google Chat space, for a Google Chat org.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
    /// The org's secret lookup order (#10423, DOC-74 §15.3): `<owner>/<repo>`
    /// (project vault, then owner vault) or `<owner>` (owner vault only).
    /// Required when the org has credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets: Option<String>,
    /// Credential refs by role (`bot_token`, `sa_key`, ...): `secret://` names.
    #[serde(default)]
    pub credentials: BTreeMap<String, String>,
}

/// One user: the org they belong to and their address on its channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserEntry {
    /// The org the user belongs to; the only org that may message them.
    pub org: String,
    /// The user's address on the org's channel (Slack member id or email).
    pub address: String,
}

/// The owner who approves every outward send (Dd9, D9a input 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerEntry {
    /// The org whose Slack carries the owner's approval DM.
    pub slack_org: String,
    /// The owner's Slack member id in that org.
    pub slack_user: String,
}

/// The whole org/user routing document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgsConfig {
    /// Org name to its channel binding.
    pub orgs: BTreeMap<String, OrgEntry>,
    /// User name (an email or handle) to their org.
    #[serde(default)]
    pub users: BTreeMap<String, UserEntry>,
    /// The approver; `None` sends every approval to the dashboard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<OwnerEntry>,
}

/// Why a config was refused. Messages name keys, never credential values.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// The YAML did not parse into the schema.
    #[error("orgs config malformed: {0}")]
    Malformed(String),
    /// A credential ref is not a `secret://` name within the org's scope.
    #[error("org {org:?} credential {role:?} is not a usable secret:// name ({code})")]
    NotSecretRef {
        /// The org.
        org: String,
        /// The credential role.
        role: String,
        /// The fixed refusal code from [`crate::channels::secret_ref::parse_in_scope`].
        code: &'static str,
    },
    /// A user names an org the config does not define.
    #[error("user {user:?} names unknown org {org:?}")]
    UnknownOrg {
        /// The user.
        user: String,
        /// The org named.
        org: String,
    },
    /// A user's address does not fit their org's channel kind.
    #[error("user {user:?} address does not fit a {kind:?} org")]
    AddressKind {
        /// The user.
        user: String,
        /// The org's kind.
        kind: ChannelKind,
    },
    /// The owner's approval org is missing or is not a Slack org.
    #[error("owner approval org {0:?} is not a configured slack org")]
    OwnerOrg(String),
}

/// Why a send has no route. Every arm sends nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    /// The user is not in the config.
    #[error("no_route: user is not configured")]
    UnknownUser,
    /// The send names an org other than the user's own.
    #[error("cross_org: the user belongs to {user_org:?}, not {via_org:?}")]
    CrossOrg {
        /// The user's org.
        user_org: String,
        /// The org the send asked for.
        via_org: String,
    },
    /// The user's org is missing from the config.
    #[error("no_route: the user's org is not configured")]
    UnknownOrg,
}

impl RouteError {
    /// The content-free rule code for audit lines.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownUser | Self::UnknownOrg => "no_route",
            Self::CrossOrg { .. } => "cross_org",
        }
    }
}

/// The one channel that may carry a message to a user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgRoute {
    /// The user's org.
    pub org: String,
    /// The org's channel kind.
    pub kind: ChannelKind,
    /// The user's address on that channel.
    pub address: String,
    /// The org's secret lookup order (its `secrets` field).
    pub secrets: Option<String>,
    /// The org's credential refs (`secret://` names).
    pub credentials: BTreeMap<String, String>,
}

/// Where an approval request goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalTarget {
    /// The owner's Slack DM: post with `channel = user` in `org`.
    OwnerSlackDm {
        /// The Slack org whose bot posts the DM.
        org: String,
        /// The owner's member id.
        user: String,
    },
    /// The dashboard, when no owner DM is configured.
    Dashboard,
}

impl OrgsConfig {
    /// Parse and validate `text`.
    ///
    /// Why: a bad ref or a dangling user must fail at load, not at send.
    /// What: serde_yaml into the schema, then [`OrgsConfig::validate`]. The
    /// serde message is reduced to its location so no value is echoed.
    /// Test: `initial_config_parses`, `non_secret_credential_is_refused`.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let cfg: Self = serde_yaml::from_str(text).map_err(|e| {
            ConfigError::Malformed(
                e.location()
                    .map_or_else(|| "schema".to_owned(), |l| format!("line {}", l.line())),
            )
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Parse and validate the daemon's `orgs.toml` `text` (#10423).
    ///
    /// What: the same schema as [`OrgsConfig::parse`], in TOML; the error is
    /// reduced to its line so no value is echoed.
    /// Test: `toml_config_parses_like_yaml`.
    pub fn parse_toml(text: &str) -> Result<Self, ConfigError> {
        let cfg: Self = toml::from_str(text).map_err(|e| {
            ConfigError::Malformed(e.span().map_or_else(
                || "schema".to_owned(),
                |s| {
                    let before = text.get(..s.start).unwrap_or_default();
                    format!("line {}", before.lines().count().max(1))
                },
            ))
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Check refs, user orgs, address shapes and the owner org.
    ///
    /// Test: `non_secret_credential_is_refused`, `user_with_unknown_org_is_refused`,
    /// `address_must_fit_the_org_kind`.
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (org, entry) in &self.orgs {
            for (role, r) in &entry.credentials {
                // #10423: DOC-74 §15.3 — refs stay within the org's own vaults.
                if let Err(code) =
                    crate::channels::secret_ref::parse_in_scope(entry.secrets.as_deref(), r)
                {
                    return Err(ConfigError::NotSecretRef {
                        org: org.clone(),
                        role: role.clone(),
                        code,
                    });
                }
            }
        }
        for (user, entry) in &self.users {
            let org = self
                .orgs
                .get(&entry.org)
                .ok_or_else(|| ConfigError::UnknownOrg {
                    user: user.clone(),
                    org: entry.org.clone(),
                })?;
            if !address_fits(org.kind, &entry.address) {
                return Err(ConfigError::AddressKind {
                    user: user.clone(),
                    kind: org.kind,
                });
            }
        }
        if let Some(owner) = &self.owner {
            let ok = self
                .orgs
                .get(&owner.slack_org)
                .is_some_and(|o| o.kind == ChannelKind::Slack)
                && address_fits(ChannelKind::Slack, &owner.slack_user);
            if !ok {
                return Err(ConfigError::OwnerOrg(owner.slack_org.clone()));
            }
        }
        Ok(())
    }

    /// The route to `user`, optionally pinned to `via_org`.
    ///
    /// Why: acceptance 4 and 8: no route is a denial, and a message goes
    /// only through the recipient's own org channel.
    /// What: `UnknownUser` for a user not in `users`; `CrossOrg` when
    /// `via_org` names any org but the user's; else the user's org route.
    /// Test: `cross_org_send_is_refused`, `unknown_user_has_no_route`,
    /// `either_org_binds_to_either_kind`.
    pub fn route(&self, user: &str, via_org: Option<&str>) -> Result<OrgRoute, RouteError> {
        let entry = self.users.get(user).ok_or(RouteError::UnknownUser)?;
        if let Some(via) = via_org
            && via != entry.org
        {
            return Err(RouteError::CrossOrg {
                user_org: entry.org.clone(),
                via_org: via.to_owned(),
            });
        }
        let org = self.orgs.get(&entry.org).ok_or(RouteError::UnknownOrg)?;
        Ok(OrgRoute {
            org: entry.org.clone(),
            kind: org.kind,
            address: entry.address.clone(),
            secrets: org.secrets.clone(),
            credentials: org.credentials.clone(),
        })
    }

    /// Where the approval for a send to any org goes.
    ///
    /// Why: D9a input 4 / Architect correction: the owner's Slack DM for
    /// every target org; no per-org approval channel without a new ruling.
    /// What: `OwnerSlackDm` when an owner is configured, else `Dashboard`.
    /// The target org is deliberately not an input.
    /// Test: `approvals_go_to_the_owner_dm_for_every_org`.
    pub fn approval_target(&self) -> ApprovalTarget {
        match &self.owner {
            Some(o) => ApprovalTarget::OwnerSlackDm {
                org: o.slack_org.clone(),
                user: o.slack_user.clone(),
            },
            None => ApprovalTarget::Dashboard,
        }
    }

    /// The org entry named `org`.
    pub fn org(&self, org: &str) -> Option<&OrgEntry> {
        self.orgs.get(org)
    }
}

/// True when `address` has the shape `kind` addresses a user by.
fn address_fits(kind: ChannelKind, address: &str) -> bool {
    match kind {
        ChannelKind::Slack => {
            address.len() > 1
                && matches!(address.as_bytes()[0], b'U' | b'W')
                && address[1..]
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        }
        ChannelKind::Gchat => {
            let mut parts = address.splitn(2, '@');
            matches!((parts.next(), parts.next()), (Some(l), Some(d)) if !l.is_empty() && d.contains('.'))
        }
    }
}
