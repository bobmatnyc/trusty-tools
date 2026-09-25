//! Per-profile precedence between the project-level and user-level token
//! stores (#8539).
//!
//! Why: `load()` used to let the project-level entry win unconditionally. A
//! project entry minted before a re-consent — unexpired, but lacking a newly
//! granted scope — then silently shadowed the fresh user-level token: reads
//! worked and Gmail filter writes got 403 `ACCESS_TOKEN_SCOPE_INSUFFICIENT`.
//! What: A pure decision function, [`resolve`], that picks the winning store
//! for one profile present in both, plus the reason it won. No I/O, no
//! logging; the caller in `storage/mod.rs` logs and routes writes.
//! Test: the `tests` module at the bottom of this file.

use std::collections::BTreeSet;
use std::fmt;

use chrono::{DateTime, Utc};

use crate::api::auth::models::StoredToken;

/// Which of the two token stores an entry was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Store {
    /// `./.gworkspace-mcp/tokens.json`, resolved from the startup cwd.
    Project,
    /// `~/.gworkspace-mcp/tokens.json`.
    User,
}

impl fmt::Display for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Store::Project => "project-level",
            Store::User => "user-level",
        })
    }
}

/// Why one store's entry won over the other's for the same profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Reason {
    /// Both entries name an account email and the emails differ: the project
    /// store is a deliberate per-directory account override.
    DifferentAccount,
    /// The winner's scope set is a strict superset of the loser's; carries
    /// the scopes only the winner holds.
    WiderScopes(Vec<String>),
    /// Scopes are equal, incomparable, or unrecorded, and the winner was
    /// issued or refreshed later.
    NewerIssue,
    /// Nothing distinguishes the two entries; the project override keeps the
    /// documented project-over-user contract.
    Tie,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reason::DifferentAccount => f.write_str(
                "the entries name different accounts; the project override is deliberate",
            ),
            Reason::WiderScopes(extra) => {
                write!(f, "wider scopes; the other entry lacks {}", extra.join(" "))
            }
            Reason::NewerIssue => f.write_str("issued or refreshed more recently"),
            Reason::Tie => {
                f.write_str("same scopes and issue time; project overrides user on a tie")
            }
        }
    }
}

/// The store whose entry a profile resolves to, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Resolution {
    pub(super) winner: Store,
    pub(super) reason: Reason,
}

/// Pick which store's entry serves `profile` when both stores hold one.
///
/// Why: The owner ruling on #8539 — the newer or wider-scoped token wins,
/// instead of the project entry winning unconditionally.
/// What: Applies these rules in order; the first that decides wins.
///
/// 1. Different account: both `metadata.email` values are present and differ
///    (case-insensitive) → project. A per-directory account override must
///    never be swapped for another mailbox's token. A missing email skips
///    this rule.
/// 2. Wider scopes: both scope lists are non-empty and one is a strict
///    superset of the other → the superset. Scope beats recency because a
///    narrower token fails writes however new it is, while an older, wider
///    token works after one refresh. An empty scope list means "not
///    recorded" and skips this rule rather than counting as the empty grant.
/// 3. Newer issue: the later [`issued_at`] wins.
/// 4. Tie → project, keeping the documented project-over-user contract where
///    nothing distinguishes the entries.
///
/// Test: `different_accounts_keep_project`, `wider_user_scopes_beat_newer_project`,
/// `wider_project_scopes_beat_newer_user`, `unrecorded_scopes_fall_back_to_recency`,
/// `incomparable_scopes_fall_back_to_recency`, `full_tie_keeps_project`.
pub(super) fn resolve(project: &StoredToken, user: &StoredToken) -> Resolution {
    if let (Some(p), Some(u)) = (&project.metadata.email, &user.metadata.email)
        && !p.eq_ignore_ascii_case(u)
    {
        return Resolution {
            winner: Store::Project,
            reason: Reason::DifferentAccount,
        };
    }

    let p_scopes = scope_set(project);
    let u_scopes = scope_set(user);
    if !p_scopes.is_empty() && !u_scopes.is_empty() && p_scopes != u_scopes {
        if u_scopes.is_superset(&p_scopes) {
            return wider(Store::User, &u_scopes, &p_scopes);
        }
        if p_scopes.is_superset(&u_scopes) {
            return wider(Store::Project, &p_scopes, &u_scopes);
        }
    }

    let (p_at, u_at) = (issued_at(project), issued_at(user));
    let (winner, reason) = if u_at > p_at {
        (Store::User, Reason::NewerIssue)
    } else if p_at > u_at {
        (Store::Project, Reason::NewerIssue)
    } else {
        (Store::Project, Reason::Tie)
    };
    Resolution { winner, reason }
}

/// When an entry's current token was issued: the later of `created_at`
/// (set at consent) and `last_refreshed` (set on refresh). `created_at` is a
/// required field, so a missing `last_refreshed` falls back to it.
pub(super) fn issued_at(entry: &StoredToken) -> DateTime<Utc> {
    entry
        .metadata
        .last_refreshed
        .map_or(entry.metadata.created_at, |r| {
            r.max(entry.metadata.created_at)
        })
}

fn scope_set(entry: &StoredToken) -> BTreeSet<&str> {
    entry.token.scopes.iter().map(String::as_str).collect()
}

fn wider(winner: Store, wide: &BTreeSet<&str>, narrow: &BTreeSet<&str>) -> Resolution {
    let extra = wide.difference(narrow).map(|s| (*s).to_string()).collect();
    Resolution {
        winner,
        reason: Reason::WiderScopes(extra),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::auth::models::{OAuthToken, TokenMetadata};
    use chrono::Duration;

    const BASE: &str = "https://www.googleapis.com/auth/gmail.modify";
    const EXTRA: &str = "https://www.googleapis.com/auth/gmail.settings.basic";

    /// An entry issued `issued_secs_ago` seconds ago holding `scopes`.
    fn entry(scopes: &[&str], issued_secs_ago: i64) -> StoredToken {
        StoredToken {
            version: 1,
            metadata: TokenMetadata {
                service_name: "work".into(),
                provider: "google".into(),
                created_at: Utc::now() - Duration::seconds(issued_secs_ago),
                last_refreshed: None,
                email: Some("user@example.com".into()),
                is_default: false,
            },
            token: OAuthToken {
                access_token: "fixture-access".into(),
                refresh_token: Some("fixture-refresh".into()),
                expires_at: Utc::now() + Duration::seconds(3600),
                scopes: scopes.iter().map(|s| (*s).to_string()).collect(),
                token_type: "Bearer".into(),
            },
        }
    }

    #[test]
    fn different_accounts_keep_project() {
        let project = entry(&[BASE], 600);
        let mut user = entry(&[BASE, EXTRA], 10);
        user.metadata.email = Some("other@example.com".into());
        let r = resolve(&project, &user);
        assert_eq!(r.winner, Store::Project);
        assert_eq!(r.reason, Reason::DifferentAccount);
    }

    #[test]
    fn wider_user_scopes_beat_newer_project() {
        // The #8539 shape: the project entry was refreshed after the
        // re-consent, so it is newer, but it lacks the new scope.
        let mut project = entry(&[BASE], 7200);
        project.metadata.last_refreshed = Some(Utc::now());
        let user = entry(&[BASE, EXTRA], 600);
        let r = resolve(&project, &user);
        assert_eq!(r.winner, Store::User);
        assert_eq!(r.reason, Reason::WiderScopes(vec![EXTRA.to_string()]));
    }

    #[test]
    fn wider_project_scopes_beat_newer_user() {
        let project = entry(&[BASE, EXTRA], 7200);
        let user = entry(&[BASE], 10);
        assert_eq!(resolve(&project, &user).winner, Store::Project);
    }

    #[test]
    fn unrecorded_scopes_fall_back_to_recency() {
        let project = entry(&[], 10);
        let user = entry(&[BASE, EXTRA], 600);
        let r = resolve(&project, &user);
        assert_eq!(r.winner, Store::Project);
        assert_eq!(r.reason, Reason::NewerIssue);
    }

    #[test]
    fn incomparable_scopes_fall_back_to_recency() {
        let project = entry(&[BASE], 600);
        let user = entry(&[EXTRA], 10);
        let r = resolve(&project, &user);
        assert_eq!(r.winner, Store::User);
        assert_eq!(r.reason, Reason::NewerIssue);
    }

    #[test]
    fn full_tie_keeps_project() {
        let project = entry(&[BASE], 600);
        let mut user = project.clone();
        user.token.access_token = "fixture-access-2".into();
        let r = resolve(&project, &user);
        assert_eq!(r.winner, Store::Project);
        assert_eq!(r.reason, Reason::Tie);
    }
}
