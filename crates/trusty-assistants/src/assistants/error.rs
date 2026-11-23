//! Typed errors for the per-assistant home model (#4325).
//!
//! Why: The home directory is USER-FACING and externally modified by design
//! (#4325's "Design Rationale: Visibility and Resilience"), so its failures are
//! things a concierge has to EXPLAIN, not opaque strings a caller can only
//! bubble. Matchable variants let the narration layer say "your home directory
//! has no `$HOME` to hang off" differently from "`izzie/..` is not a legal
//! instance id" without string-sniffing an `anyhow::Error`.
//! What: [`AssistantError`] — the errors raised while RESOLVING or CREATING a
//! home. Errors while INSPECTING an existing home are not errors at all: they
//! are findings, and live in [`super::health`] instead, because a malformed
//! home must never fail a caller that only wanted to report on it.
//! Test: `super::tests::instance_tests::rejects_path_separators`,
//! `super::tests::home_tests::ensure_is_idempotent`.

use std::path::PathBuf;

use thiserror::Error;

/// A failure resolving or creating an assistant instance's home directory.
///
/// Why/What/Test: see this module's doc comment.
#[derive(Debug, Error)]
pub enum AssistantError {
    /// #4325: the instance id would not be safe as a single directory name.
    #[error("`{raw}` is not a usable assistant instance id: {reason}")]
    InvalidInstanceId { raw: String, reason: String },

    /// #4325: no `$HOME`/`$USERPROFILE`, so the dotless assistants root under
    /// the user's home directory cannot be located.
    #[error(
        "cannot locate the assistants root: neither $HOME nor $USERPROFILE is set \
         (set ${env} to choose one explicitly)"
    )]
    NoUserHome { env: &'static str },

    /// #4325: a `[[stores]] root` that would escape the assistant's own home.
    #[error("store `{store}` declares root `{root}`, which {reason}")]
    UnconfinedStoreRoot {
        store: String,
        root: String,
        reason: String,
    },

    /// #4325: creating or seeding one entry of the home failed.
    #[error("could not create `{}`: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
