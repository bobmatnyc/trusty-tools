//! The one caller-facing "palace not found" error (#9751).
//!
//! Why: a write to a missing palace answered `open palace X: load palace
//! metadata for X: palace metadata missing at <root>/X/palace.json` — the
//! daemon's own filesystem path, and no hint of the palaces that do exist.
//! Thirteen writes in 72 h failed that way and the callers dropped them.
//! What: [`PalaceNotFound`] is the error `PalaceRegistry::open_palace` raises
//! for a genuine absence, and the text every surface (MCP tool, wire method,
//! service error) relays. It never holds a path; the registry logs the path
//! server-side at debug.
//! Test: `palace_not_found_tests` here, plus one test per surface:
//! `memory_remember_and_note_name_a_missing_palace_without_its_path`,
//! `rpc_missing_palace_is_named_with_existing_palaces_and_no_path`,
//! `absent_palace_is_named_with_existing_palaces_and_no_path_at_every_service_site`.

use crate::engine::registry::PalaceRegistry;
use std::fmt;
use std::path::Path;

/// Most existing palace ids one not-found message lists.
pub const MAX_LISTED_PALACES: usize = 20;

/// Which existing palaces a [`PalaceNotFound`] message names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExistingPalaces {
    /// Not enumerated. The registry raises the error this way, so an open on a
    /// hot path never pays for a directory walk; a surface adds the listing.
    NotListed,
    /// The first [`MAX_LISTED_PALACES`] ids in sorted order, and how many
    /// palaces exist in total.
    Listed {
        /// The ids shown, sorted, at most [`MAX_LISTED_PALACES`].
        shown: Vec<String>,
        /// Every palace under the root, including the ones not shown.
        total: usize,
    },
    /// Listing the palaces failed; the message says so and lists none.
    Unavailable,
}

/// A palace id names no palace on disk.
///
/// Why: see the module doc — the caller needs the id it sent and the ids it
/// could have sent, and must never see the daemon's filesystem layout.
/// What: `Display` is the whole caller-facing message, built here only:
/// - `palace not found: <id>` when not listed;
/// - `palace not found: <id>; existing palaces (<n> of <total> shown): a, b`
///   when listed, where `n <= 20` and `n == total` unless the cap applies;
/// - `palace not found: <id>; no palaces exist` when the root holds none;
/// - `palace not found: <id>; the existing palaces could not be listed` when
///   the listing failed.
///
/// The ids are palace ids — the value a caller passes as `palace` — never
/// paths. `PalaceRegistry::open_error_is_absent` matches this type.
/// Test: `listing_is_sorted_capped_at_twenty_and_counted`,
/// `an_empty_root_says_no_palaces_exist`,
/// `a_failed_listing_is_still_not_found_and_lists_nothing`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct PalaceNotFound {
    palace: String,
    existing: ExistingPalaces,
}

impl PalaceNotFound {
    /// The absence of `palace`, without a listing (the registry's form).
    pub fn new(palace: impl Into<String>) -> Self {
        Self {
            palace: palace.into(),
            existing: ExistingPalaces::NotListed,
        }
    }

    /// The absence of `palace`, listing the ids in `ids` (sorted, deduplicated,
    /// capped at [`MAX_LISTED_PALACES`]).
    pub fn from_ids(palace: impl Into<String>, ids: impl IntoIterator<Item = String>) -> Self {
        let mut ids: Vec<String> = ids.into_iter().collect();
        ids.sort();
        ids.dedup();
        let total = ids.len();
        ids.truncate(MAX_LISTED_PALACES);
        Self {
            palace: palace.into(),
            existing: ExistingPalaces::Listed { shown: ids, total },
        }
    }

    /// The absence of `palace`, listing the palaces under `data_root`.
    ///
    /// A listing failure keeps the error a not-found and omits the list; the
    /// failure, which may name a path, goes to the server log only.
    pub fn listing(palace: impl Into<String>, data_root: &Path) -> Self {
        let palace = palace.into();
        match PalaceRegistry::list_palaces(data_root) {
            Ok(palaces) => Self::from_ids(palace, palaces.into_iter().map(|p| p.id.0)),
            Err(e) => {
                // #9751: the listing error can carry a path; log it, never relay it.
                tracing::warn!(palace = %palace, "#9751: cannot list palaces for a not-found message: {e:#}");
                Self {
                    palace,
                    existing: ExistingPalaces::Unavailable,
                }
            }
        }
    }

    /// The listed form of `err` when it is a palace absence, else `None`.
    ///
    /// The one place a surface turns the registry's bare absence into the
    /// caller-facing message, so every surface lists the same way.
    pub fn listed_absence(err: &anyhow::Error, data_root: &Path) -> Option<Self> {
        err.downcast_ref::<Self>()
            .map(|absent| Self::listing(absent.palace.clone(), data_root))
    }

    /// The palace id that was not found.
    pub fn palace(&self) -> &str {
        &self.palace
    }

    /// The existing palaces this error names.
    pub fn existing(&self) -> &ExistingPalaces {
        &self.existing
    }
}

impl fmt::Display for PalaceNotFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "palace not found: {}", self.palace)?;
        match &self.existing {
            ExistingPalaces::NotListed => Ok(()),
            ExistingPalaces::Listed { total: 0, .. } => write!(f, "; no palaces exist"),
            ExistingPalaces::Listed { shown, total } => write!(
                f,
                "; existing palaces ({} of {total} shown): {}",
                shown.len(),
                shown.join(", ")
            ),
            ExistingPalaces::Unavailable => write!(f, "; the existing palaces could not be listed"),
        }
    }
}
