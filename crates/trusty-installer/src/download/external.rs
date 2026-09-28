//! Tools whose prebuilts the installer fetches but which are built and
//! released OUTSIDE this workspace.
//!
//! Why: `tga` and `trusty-audit` left the trusty-tools workspace (owner ruling
//! 2026-09-28). They build, tag and publish from `bobmatnyc/trusty-git-analytics`,
//! yet the installer still fetches their prebuilts. Their origin therefore has to be a row the
//! installer owns, not the assumption that every installable crate sits under
//! this repo's `crates/`.
//!
//! What: [`EXTERNAL_TOOLS`] holds one row per external tool: its crates.io
//! package (the `cargo install` fallback and the `tctl updates` version source),
//! the GitHub repo hosting its release assets, any extra release-tag spelling,
//! and its asset filename prefix. [`external_tool`] finds a row under any
//! spelling. Release routing, tag aliases and asset names in
//! [`super::release`] read this table. A row does not make a tool installable
//! by name; only a `commands::stable_set::stable_set` row does, and `tga` has
//! one while `trusty-audit` does not (ruling 2026-09-28 00:25Z).
//!
//! Test: `tests::external_tools_publish_from_trusty_git_analytics`,
//! `tests::external_tool_resolves_under_every_spelling`,
//! `tests::workspace_crates_are_not_external`.

use super::release::{ReleaseRepo, TRUSTY_GIT_ANALYTICS_REPO};

/// One tool built and released outside the trusty-tools workspace.
///
/// Why: installing a tool needs four facts that a workspace member gets from
/// this repo's conventions; an external tool has to state them.
/// What: see each field.
/// Test: `tests::external_tools_publish_from_trusty_git_analytics`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExternalTool {
    /// crates.io package name — `cargo install <crate_name> --locked`.
    pub crate_name: &'static str,
    /// GitHub repo whose releases carry the prebuilt tarballs.
    pub repo: ReleaseRepo,
    /// Other `<name>-v*` tag spellings the release pipeline may use.
    pub tag_aliases: &'static [&'static str],
    /// Filename prefix of the release tarball (`<prefix>-<version>-<target>`).
    pub asset_prefix: &'static str,
}

/// Every external tool whose prebuilts the installer fetches.
///
/// `tga`'s tarball and one tag spelling carry its former directory name,
/// `trusty-git-analytics` (#6771). Add a row when another crate leaves the
/// workspace.
pub(crate) const EXTERNAL_TOOLS: &[ExternalTool] = &[
    ExternalTool {
        crate_name: "tga",
        repo: TRUSTY_GIT_ANALYTICS_REPO,
        tag_aliases: &["trusty-git-analytics"],
        asset_prefix: "trusty-git-analytics",
    },
    ExternalTool {
        crate_name: "trusty-audit",
        repo: TRUSTY_GIT_ANALYTICS_REPO,
        tag_aliases: &[],
        asset_prefix: "trusty-audit",
    },
];

/// The external tool `name` refers to — its crate name or a tag alias.
///
/// Why: a pin or a release lookup may spell `tga` as `trusty-git-analytics`,
/// and both spellings must reach the same row.
/// What: `Some(row)` when `name` is a row's `crate_name` or one of its
/// `tag_aliases`; `None` for every workspace crate.
/// Test: `tests::external_tool_resolves_under_every_spelling`,
/// `tests::workspace_crates_are_not_external`.
pub(crate) fn external_tool(name: &str) -> Option<&'static ExternalTool> {
    EXTERNAL_TOOLS
        .iter()
        .find(|t| t.crate_name == name || t.tag_aliases.contains(&name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the ruling moves both tools to one repo; a row naming another
    /// repo would fetch prebuilts from where they are no longer published.
    /// What: every row's repo is `bobmatnyc/trusty-git-analytics`, and the
    /// table holds exactly `tga` and `trusty-audit`.
    /// Test: This is the test.
    #[test]
    fn external_tools_publish_from_trusty_git_analytics() {
        let names: Vec<&str> = EXTERNAL_TOOLS.iter().map(|t| t.crate_name).collect();
        assert_eq!(names, ["tga", "trusty-audit"]);
        for t in EXTERNAL_TOOLS {
            assert_eq!(
                t.repo.slug, "bobmatnyc/trusty-git-analytics",
                "{}",
                t.crate_name
            );
        }
    }

    /// Why: `tga` is tagged under two spellings (#6771).
    /// What: both spellings reach the `tga` row; `trusty-audit` reaches its own.
    /// Test: This is the test.
    #[test]
    fn external_tool_resolves_under_every_spelling() {
        for name in ["tga", "trusty-git-analytics"] {
            assert_eq!(external_tool(name).map(|t| t.crate_name), Some("tga"));
        }
        assert_eq!(
            external_tool("trusty-audit").map(|t| t.crate_name),
            Some("trusty-audit")
        );
    }

    /// Why: a workspace crate routed through this table would fetch from the
    /// wrong repo.
    /// What: the in-workspace stable-set crates have no row.
    /// Test: This is the test.
    #[test]
    fn workspace_crates_are_not_external() {
        for name in ["trusty-search", "trusty-analyze", "trusty-installer"] {
            assert!(external_tool(name).is_none(), "{name}");
        }
    }
}
