//! From validated `sources[]` entries to runnable drain passes (#6657).
//!
//! Why: a host drains several projects, each to its own object store, and the
//! key every upload lands under is `<owner>/<project>/…`. Deciding which
//! project a source belongs to is the step that must never guess: a wrong
//! answer files one team's logs under another's prefix, in an account that may
//! not be theirs. It lives beside the config parser rather than inside it
//! because it reads git, while parsing is pure — and because the parent module
//! is close to the 500-SLOC production cap.
//!
//! What: [`build`] resolves each enabled source's [`DrainTarget`], groups the
//! sources by the `(destination, target)` pair they resolved to, and returns
//! the disabled ones alongside so the doctor row can name them.
//!
//! Test: `super::tests` — the resolution matrix is driven through
//! `resolve_log_drain`, over real temp git repos.

use trusty_common::github_path::derive_remote_repo;
use trusty_common::log_drain::DrainTarget;

use super::{LogDrainConfigError, NamedDestination, PreparedSource, ResolvedDrainDestination};

/// A source the operator turned off with `enabled: false`.
///
/// Why: dropping it silently would make `tm doctor` report a project as
/// undrained with no way to tell "switched off" from "never configured".
/// What: what the row needs and nothing else — the source's `crate_name` and
/// the destination it WOULD have used, when one is known.
/// Test: `super::tests::resolve_skips_a_disabled_source`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DisabledSource {
    /// The entry's `crate_name`.
    pub crate_name: String,
    /// The destination it would have drained to, as the operator wrote it.
    pub destination_display: Option<String>,
}

/// Group enabled sources into passes, and collect the disabled ones.
///
/// Why: one place decides both the destination and the project of every source,
/// so the scheduler and the doctor row can never disagree about either.
/// What: for each enabled source, resolves its destination (its own, else
/// `default`) and its identity (its own, else the git `origin` of its root,
/// else `fallback`), then appends it to the pass with the same pair. Groups
/// keep first-appearance order; a linear scan is right because a host drains to
/// a handful of passes and `DestinationUri` is `Eq` but not `Hash`.
/// Test: `super::tests::resolve_groups_sources_by_destination`,
/// `super::tests::resolve_splits_one_destination_by_project`,
/// `super::tests::resolve_skips_a_disabled_source`.
///
/// # Errors
/// [`LogDrainConfigError::MissingDestination`] for a source with no destination
/// at all; [`LogDrainConfigError::SourceIdentity`] for one whose owner and
/// project cannot be resolved. Neither is recoverable by guessing — see the
/// module docs.
pub(super) fn build(
    prepared: Vec<PreparedSource>,
    default: Option<NamedDestination>,
    fallback: Option<&DrainTarget>,
) -> Result<(Vec<ResolvedDrainDestination>, Vec<DisabledSource>), LogDrainConfigError> {
    let mut groups: Vec<ResolvedDrainDestination> = Vec::new();
    let mut disabled = Vec::new();

    for entry in prepared {
        let named = entry.destination.clone().or_else(|| default.clone());
        if !entry.enabled {
            disabled.push(DisabledSource {
                crate_name: entry.source.crate_name.clone(),
                destination_display: named.map(|(display, _)| display),
            });
            continue;
        }

        // A source with no override needs the section default; without one
        // there is nowhere for it to go, and guessing is what #6657 forbids.
        let Some((display, uri)) = named else {
            return Err(LogDrainConfigError::MissingDestination);
        };
        let target = resolve_target(&entry, fallback)?;

        match groups
            .iter_mut()
            .find(|g| g.destination == uri && g.target == target)
        {
            Some(group) => group.sources.push(entry.source),
            None => groups.push(ResolvedDrainDestination {
                destination: uri,
                destination_display: display,
                target,
                sources: vec![entry.source],
            }),
        }
    }

    Ok((groups, disabled))
}

/// Decide the `<owner>/<project>` one source's keys sit under.
///
/// Order: the entry's own `owner`/`project`, then the git `origin` of its
/// `root`, then the section-level fallback. The repo beats the section default
/// because it is the more specific statement about whose logs these are; the
/// section default exists for roots no repo owns, such as the daemon's own
/// `~/.trusty-mpm/logs`.
///
/// `derive_remote_repo` runs `git -C <root> config --get remote.origin.url`, so
/// a log directory INSIDE a checkout resolves to that checkout.
///
/// # Errors
/// [`LogDrainConfigError::SourceIdentity`] carrying git's own reason, so the
/// operator sees whether the root is not a repo, has no origin, or has an
/// origin that does not parse.
fn resolve_target(
    entry: &PreparedSource,
    fallback: Option<&DrainTarget>,
) -> Result<DrainTarget, LogDrainConfigError> {
    if let Some(explicit) = entry.identity.clone() {
        return Ok(explicit);
    }
    let probe = match derive_remote_repo(&entry.source.root) {
        Ok(remote) => {
            return Ok(DrainTarget {
                owner: remote.owner,
                project: remote.repo,
            });
        }
        Err(e) => e.to_string(),
    };
    fallback
        .cloned()
        .ok_or_else(|| LogDrainConfigError::SourceIdentity {
            index: entry.index,
            crate_name: entry.source.crate_name.clone(),
            root: entry.source.root.display().to_string(),
            reason: probe,
        })
}
