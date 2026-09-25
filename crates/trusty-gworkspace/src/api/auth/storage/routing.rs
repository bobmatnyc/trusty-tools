//! Where each change from a token-store update is written (#8539).
//!
//! Why: Split out of `storage/mod.rs` to keep it under the 500-SLOC cap, and
//! to keep the routing rule a pure function with no I/O.
//! What: [`Tiers`] (both stores as read, plus the merged view) and
//! [`route_writes`], which splits an updated merged view back into the two
//! stores.
//! Test: the routing tests in `storage/tests.rs` and `oauth/flow/tests.rs`,
//! named in [`route_writes`]'s doc.

use std::collections::HashMap;

use super::precedence::{Store, same_account};
use crate::api::auth::models::StoredToken;

/// Both stores as read from disk, plus the merged view `load` serves and the
/// store each merged entry came from.
pub(super) struct Tiers {
    pub(super) user: HashMap<String, StoredToken>,
    pub(super) project: HashMap<String, StoredToken>,
    pub(super) merged: HashMap<String, StoredToken>,
    pub(super) origin: HashMap<String, Store>,
}

/// The two stores' new content, plus the profiles whose user-level entry a
/// removal kept because it names a different or unrecorded account.
pub(super) struct Routed {
    pub(super) user: HashMap<String, StoredToken>,
    pub(super) project: HashMap<String, StoredToken>,
    pub(super) kept_user_entries: Vec<String>,
}

/// Split an updated merged view back into the two stores.
///
/// Why: Writing the merged view to one file copied the other store's entries
/// into it and sent a user-level winner's refresh to the project store
/// (#8539). A pure function keeps the routing rule in one place.
/// What: Starts from each store's on-disk content, then:
///
/// - A profile dropped from the merged view is removed from the store that
///   served it. The other store's entry is removed too only when it names the
///   same account ([`same_account`]); a different or unrecorded account is
///   kept, so a removal never destroys another account's credential. A kept
///   entry loses `is_default` when the merged view already has a default, so
///   two defaults never result.
/// - An entry equal to its pre-update value is left where it is, never copied
///   into the other store.
/// - The `consent` profile's entry goes to `new_profiles` (project if one
///   exists, else user). With a project store, it also replaces the user
///   entry when that entry names the same account, so other directories stop
///   serving the old token; a different or unrecorded account stays
///   project-only.
/// - Any other changed entry goes back to the store it was read from; a
///   profile new to both stores goes to `new_profiles`.
///
/// Refresh races are settled by the caller under the lock, not here: see
/// `OAuthManager::refresh`.
/// Test: `refresh_write_back_targets_the_winning_store`,
/// `remove_profile_clears_both_stores`,
/// `remove_profile_keeps_a_different_account_user_entry`,
/// `remove_profile_never_leaves_two_defaults`,
/// `persist_in_project_dir_does_not_overwrite_user_credential`,
/// `persist_same_account_in_project_dir_updates_both_stores`,
/// `persist_narrower_same_account_consent_replaces_wider_user_entry`.
pub(super) fn route_writes(
    tiers: &Tiers,
    merged: &HashMap<String, StoredToken>,
    new_profiles: Store,
    consent: Option<&str>,
) -> Routed {
    let mut user = tiers.user.clone();
    let mut project = tiers.project.clone();
    let mut kept_user_entries = Vec::new();
    let merged_has_default = merged.values().any(|e| e.metadata.is_default);
    for (profile, old) in &tiers.merged {
        if merged.contains_key(profile) {
            continue;
        }
        let served_by_user = tiers.origin.get(profile) == Some(&Store::User);
        let (served, other) = if served_by_user {
            (&mut user, &mut project)
        } else {
            (&mut project, &mut user)
        };
        served.remove(profile);
        match other.get(profile).map(|o| same_account(o, old)) {
            // #8539: never delete another account's credential with this one.
            Some(true) => {
                other.remove(profile);
            }
            Some(false) => {
                // #8539: the kept entry must not become a second default.
                if merged_has_default && let Some(kept) = other.get_mut(profile) {
                    kept.metadata.is_default = false;
                }
                if !served_by_user {
                    kept_user_entries.push(profile.clone());
                }
            }
            None => {}
        }
    }
    for (profile, entry) in merged {
        if tiers.merged.get(profile) == Some(entry) {
            continue;
        }
        if consent == Some(profile.as_str()) {
            // #8539: a consent is new; mirror it to a same-account user entry.
            if new_profiles == Store::Project {
                project.insert(profile.clone(), entry.clone());
                if tiers
                    .user
                    .get(profile)
                    .is_some_and(|u| same_account(u, entry))
                {
                    user.insert(profile.clone(), entry.clone());
                }
            } else {
                user.insert(profile.clone(), entry.clone());
            }
            continue;
        }
        // #8539: a refresh or metadata change goes back to its origin store.
        match tiers.origin.get(profile).copied().unwrap_or(new_profiles) {
            Store::User => user.insert(profile.clone(), entry.clone()),
            Store::Project => project.insert(profile.clone(), entry.clone()),
        };
    }
    Routed {
        user,
        project,
        kept_user_entries,
    }
}
