//! The pinned tools this client runs, and the seam where they get installed.
//!
//! Why: the owner's framing is that the auditor client is an installer runner —
//! it fetches `tga`, `trusty-analyze` and `trusty-review` at pinned versions and
//! then drives them. Downloading is `trusty-installer`'s domain, and CLAUDE.md's
//! "Common entry point, clean domain demarcation" rule makes a second download
//! implementation a defect rather than a shortcut. So this module knows WHICH
//! tools are required and WHETHER they are present; it does not know how to
//! fetch one.
//!
//! What: [`RequiredTool`], the three-tool set; [`ToolStatus`], the result of
//! probing the working directory's `tools/` area; and [`install`], the seam —
//! it returns [`AuditError::NotImplemented`] citing #5491 and will be replaced
//! by a call into `trusty-installer`'s pinned, fail-closed entry point once that
//! lands. It fails closed on purpose: the fallback that must never appear here
//! is a bespoke download, and the second-worst is `trusty-installer`'s current
//! `try_install_prebuilt`, which is latest-only and fail-opens to
//! `cargo install` on a checksum mismatch (milestone 62's note on #5491).
//! Test: `super::tool_tests`.

use std::path::{Path, PathBuf};

use crate::error::AuditError;
use crate::workdir::{Area, WorkDir};

/// A tool the audit run needs on disk before it can start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RequiredTool {
    /// The audit sweep itself.
    Tga,
    /// Static analysis feeding the scorecard.
    TrustyAnalyze,
    /// Report rendering.
    TrustyReview,
}

impl RequiredTool {
    /// Every tool the run needs (#5495).
    pub const ALL: [RequiredTool; 3] = [
        RequiredTool::Tga,
        RequiredTool::TrustyAnalyze,
        RequiredTool::TrustyReview,
    ];

    /// The binary name as it lands in the working directory's `tools/` area.
    pub fn binary_name(self) -> &'static str {
        match self {
            RequiredTool::Tga => "tga",
            RequiredTool::TrustyAnalyze => "trusty-analyze",
            RequiredTool::TrustyReview => "trusty-review",
        }
    }

    /// The crate `trusty-installer` fetches to produce that binary.
    pub fn crate_name(self) -> &'static str {
        match self {
            RequiredTool::Tga => "tga",
            RequiredTool::TrustyAnalyze => "trusty-analyze",
            RequiredTool::TrustyReview => "trusty-review",
        }
    }

    /// Where this tool's binary belongs under the working directory.
    pub fn path_in(self, work: &WorkDir) -> PathBuf {
        work.path(Area::Tools).join(self.binary_name())
    }
}

/// Whether a required tool is on disk in the working directory.
///
/// Why: the guided flow's first honest answer to "can this run?" is which tools
/// are missing, and that answer is available before any download exists.
/// What: the tool plus the path, present only when the file is there.
/// Test: `super::tool_tests::status_reports_a_tool_that_is_on_disk`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolStatus {
    /// The tool probed.
    pub tool: RequiredTool,
    /// Where it would live.
    pub path: PathBuf,
    /// Whether a file is there now.
    pub installed: bool,
}

/// Probe the working directory for every required tool.
///
/// Why: pure enough to test with a temp directory, and the only thing the
/// scaffold can truthfully report about tooling.
/// What: one [`ToolStatus`] per [`RequiredTool::ALL`] entry, in that order. It
/// checks for a file's existence only — verifying a binary's version or
/// checksum is #5491's job, not a second implementation here.
/// Test: `super::tool_tests::status_reports_a_tool_that_is_on_disk`.
pub fn status(work: &WorkDir) -> Vec<ToolStatus> {
    RequiredTool::ALL
        .iter()
        .map(|tool| {
            let path = tool.path_in(work);
            ToolStatus {
                tool: *tool,
                installed: path.is_file(),
                path,
            }
        })
        .collect()
}

/// Install a pinned tool into `install_dir`. **Not implemented — see #5491.**
///
/// Why: this is the seam #5502 leaves deliberately empty. `trusty-installer`
/// owns platform detection, pinned-version resolution and SHA-256 verification;
/// #5491 is adding the fail-closed entry point this function will call. Writing
/// download code here would duplicate that domain, and duplicating it is what
/// CLAUDE.md's common-entry-point rule forbids.
/// What: today, an [`AuditError::NotImplemented`] citing #5491. When #5491
/// lands, the body becomes a call into its entry point with the pinned version
/// for `tool`, and the signature stays as it is.
/// Test: `super::tool_tests::install_fails_closed_until_5491_lands`.
///
/// # Errors
///
/// Always, until #5491 lands.
pub fn install(_tool: RequiredTool, _install_dir: &Path) -> Result<PathBuf, AuditError> {
    // #5491 SEAM: replace this body with the call into trusty-installer's
    // pinned, fail-closed prebuilt entry point. Do not add download, checksum,
    // or extraction code to this crate — that domain belongs to trusty-installer.
    Err(AuditError::NotImplemented {
        capability: "pinned tool downloads",
        issue: 5491,
    })
}

#[cfg(test)]
mod tool_tests {
    use super::*;

    #[test]
    fn status_reports_a_tool_that_is_on_disk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = WorkDir::new(tmp.path().join("work"));
        work.create().expect("create");

        std::fs::write(RequiredTool::Tga.path_in(&work), b"#!/bin/sh\n").expect("write stub");

        let statuses = status(&work);
        assert_eq!(statuses.len(), RequiredTool::ALL.len());
        let tga = statuses
            .iter()
            .find(|s| s.tool == RequiredTool::Tga)
            .expect("tga present in the report");
        assert!(tga.installed);
        assert!(
            statuses
                .iter()
                .filter(|s| s.tool != RequiredTool::Tga)
                .all(|s| !s.installed)
        );
    }

    #[test]
    fn every_tool_lands_under_the_tools_area() {
        let work = WorkDir::new("/work");
        for tool in RequiredTool::ALL {
            assert!(tool.path_in(&work).starts_with(work.path(Area::Tools)));
        }
    }

    #[test]
    fn install_fails_closed_until_5491_lands() {
        let err = install(RequiredTool::Tga, Path::new("/work/tools"))
            .expect_err("the seam must not pretend to succeed");
        assert!(matches!(
            err,
            AuditError::NotImplemented { issue: 5491, .. }
        ));
    }
}
