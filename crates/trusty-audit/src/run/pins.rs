//! The preflight that decides which four binaries a sweep may drive.
//!
//! Why: split out of `crate::run` when that file crossed the 500-SLOC
//! production cap (#5857). It is the one concern in the sweep that runs to
//! completion before any child starts and answers a single question — are the
//! installed tools the ones this engagement pins — so it separates cleanly.
//!
//! What: [`PinnedBinaries`], the four paths by name, and [`pinned_binaries`],
//! the three-condition check that produces them or refuses.
//!
//! Test: `crate::run::run_tests::a_run_without_the_pinned_tools_is_refused`,
//! `crate::run::run_tests::an_unverified_binary_does_not_count_as_installed`,
//! `crate::run::run_tests::a_binary_installed_at_a_different_pin_is_refused`.

use std::path::PathBuf;

use crate::config::ToolPins;
use crate::error::AuditError;
use crate::tool_overrides::ToolOverrides;
use crate::tools::{self, RequiredTool};
use crate::workdir::WorkDir;

/// The four binaries a run drives, each proven to be at the engagement's pin.
///
/// Why: named fields rather than a lookup table, so the "tool not found" branch
/// does not exist. The obvious table version needs a fallback arm at every use
/// site, and the natural fallback — the bare binary name — is a `PATH` lookup,
/// which is the one thing the sweep must never do.
#[derive(Debug, Clone)]
pub(super) struct PinnedBinaries {
    pub(super) tga: PathBuf,
    pub(super) search: PathBuf,
    pub(super) analyze: PathBuf,
    pub(super) review: PathBuf,
}

/// The pinned binaries this run drives, or a refusal naming what is wrong.
///
/// Why: the run must use the binaries THIS client installed and verified at the
/// version THIS engagement pins — never whatever `tga` happens to be on the
/// operator's `PATH`, and never a copy installed before the config was bumped.
/// Both are the #5454 version-skew class, and there is no fallback for either.
///
/// Three conditions, each a refusal: the file is present, the version record
/// this client wrote names it, and that recorded version equals the engagement's
/// pin. The second matters because a binary someone dropped into `tools/` by
/// hand reads as `installed` with no version — unverified is not a weaker kind
/// of installed. The third matters because install and run are separate steps,
/// so the config can change between them.
/// #6132 adds ONE branch ahead of those three, and only one: an explicit
/// operator override REPLACES the pin for that tool. It is not a fallback —
/// [`ToolOverrides::resolve`] has already proven the path executable and refused
/// the run if it could not, so nothing here can silently return to the pin. A
/// tool nobody overrode is unaffected, which is every tool on an ordinary run.
///
/// What: reads [`tools::status`], applies the override where there is one,
/// checks all three conditions where there is not, and returns the paths by
/// name.
/// Test: `crate::run::run_tests::a_run_without_the_pinned_tools_is_refused`,
/// `crate::run::run_tests::an_unverified_binary_does_not_count_as_installed`,
/// `crate::run::run_tests::a_binary_installed_at_a_different_pin_is_refused`,
/// `crate::run::run_tests::an_override_replaces_a_pin_the_client_never_installed`.
///
/// # Errors
///
/// [`AuditError::ToolsNotInstalled`] naming every non-overridden tool that is
/// missing or unverified, [`AuditError::VersionMismatch`] for the first such
/// tool whose recorded version is not the engagement's pin, and whatever
/// [`tools::status`] fails with.
pub(super) fn pinned_binaries(
    work: &WorkDir,
    pins: &ToolPins,
    overrides: &ToolOverrides,
) -> Result<PinnedBinaries, AuditError> {
    let statuses = tools::status(work)?;
    let missing: Vec<&'static str> = statuses
        .iter()
        // #6132: an overridden tool is not required to be installed at all. The
        // case this exists for is a pin that cannot be downloaded because it is
        // merged and unpublished, so demanding the install would refuse the run
        // the override enables.
        .filter(|s| overrides.path_of(s.tool).is_none())
        .filter(|s| !s.installed || s.version.is_none())
        .map(|s| s.tool.binary_name())
        .collect();
    if !missing.is_empty() {
        return Err(AuditError::ToolsNotInstalled { missing });
    }

    let path_of = |tool: RequiredTool| -> Result<PathBuf, AuditError> {
        // #6132: explicit override, then the pin. There is no third branch.
        if let Some(local) = overrides.path_of(tool) {
            return Ok(local.to_path_buf());
        }
        let pinned = tool.pin_in(pins).version();
        let status = statuses.iter().find(|s| s.tool == tool).ok_or_else(|| {
            AuditError::ToolsNotInstalled {
                missing: vec![tool.binary_name()],
            }
        })?;
        // `version` is Some: the missing check above rejected every None.
        match status.version.as_deref() {
            Some(v) if v == pinned => Ok(status.path.clone()),
            Some(v) => Err(AuditError::VersionMismatch {
                tool: tool.crate_name(),
                pinned: pinned.to_owned(),
                installed: v.to_owned(),
            }),
            None => Err(AuditError::ToolsNotInstalled {
                missing: vec![tool.binary_name()],
            }),
        }
    };

    Ok(PinnedBinaries {
        tga: path_of(RequiredTool::Tga)?,
        search: path_of(RequiredTool::TrustySearch)?,
        analyze: path_of(RequiredTool::TrustyAnalyze)?,
        review: path_of(RequiredTool::TrustyReview)?,
    })
}
