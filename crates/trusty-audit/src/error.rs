//! The crate's error type.
//!
//! Why: `trusty-audit` is a library with a thin CLI over it (#5502), so the
//! library owes callers a structured error rather than `anyhow` — the CLI, and
//! later the Tauri shell, each render failures their own way.
//! What: one `#[non_exhaustive]` enum covering what this crate can fail at —
//! touching the working directory, reading the companion files, installing the
//! pinned tools, and being asked for a capability that is not built yet.
//!
//! Every install-side variant is a REFUSAL, never a degraded success (#5495).
//! [`AuditError::Install`] keeps `trusty-installer`'s structured
//! `PinnedError` rather than flattening it to a string, because that type
//! already states whether the install directory is clean, and a front end
//! deciding what to tell the operator needs that distinction intact.
//! Test: `super::error_tests`.

use std::path::PathBuf;

/// Everything `trusty-audit`'s library surface can fail with.
///
/// Why: every variant names the path or tool it concerns, because the recipient
/// running this is not the author and a bare `io::Error` tells them nothing
/// about which of the working directory's several files was unreadable.
/// What: `#[non_exhaustive]` so adding a variant in a later milestone is not a
/// breaking change (CLAUDE.md, semver gate).
/// Test: `super::error_tests::messages_name_the_offending_path`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuditError {
    /// A working-directory entry could not be created or inspected.
    #[error("working directory {path}: {source}")]
    WorkDir {
        /// The path that failed.
        path: PathBuf,
        /// The underlying filesystem error.
        #[source]
        source: std::io::Error,
    },

    /// A companion file (engagement config, audit manifest) could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The file that failed.
        path: PathBuf,
        /// The underlying filesystem error.
        #[source]
        source: std::io::Error,
    },

    /// A companion file was read but is not valid TOML for its schema.
    #[error("{path} is not a valid {what}: {source}")]
    Parse {
        /// The file that failed.
        path: PathBuf,
        /// Which schema was expected, e.g. `"audit manifest"`.
        what: &'static str,
        /// The underlying TOML error.
        ///
        /// Boxed because `toml::de::Error` carries the full input span and is
        /// large enough on its own to trip `clippy::result_large_err` for every
        /// `Result<_, AuditError>` in the crate.
        #[source]
        source: Box<toml::de::Error>,
    },

    /// The pinned install refused. Carries `trusty-installer`'s own reason.
    ///
    /// Why: #5495 routes downloading through `trusty-installer`'s fail-closed
    /// entry point, and that entry point's error already says what was pinned,
    /// what arrived, and whether the install directory is clean. Re-describing
    /// it here would lose the one field that matters most —
    /// `PinnedError::PlacementInterrupted` names files left on disk, and a
    /// caller that flattened it to a string would report a clean directory that
    /// is not clean.
    /// What: a transparent wrapper. Boxed because `PinnedError` carries
    /// `anyhow::Error` and several `Vec`s, and an unboxed variant makes every
    /// `Result<_, AuditError>` in the crate trip `clippy::result_large_err`.
    /// Test: `crate::tools::tool_tests::install_errors_keep_the_installers_reason`.
    #[error(transparent)]
    Install {
        /// Why the pinned install refused, verbatim from `trusty-installer`.
        #[from]
        source: Box<trusty_installer::download::pinned::PinnedError>,
    },

    /// An installed binary does not report the version the config pinned.
    ///
    /// Why: `trusty-installer` proves the pin before placing anything, so this
    /// is the client's own second look at what it was handed — belt and braces
    /// on the exact defect class #5454 closed from the run side. Reaching it
    /// means the installer's postcondition did not hold, which is worth an
    /// explicit refusal rather than a silent acceptance.
    /// What: names the tool, the pin, and what came back.
    ///
    /// It is raised on the RUN side too (#5555): install and run are separate
    /// steps, so an engagement config bumped between them would otherwise run
    /// the older binary and report success.
    /// Test: `crate::tools::tool_tests::a_version_that_drifts_from_the_pin_is_refused`,
    /// `crate::run::run_tests::a_binary_installed_at_a_different_pin_is_refused`.
    #[error(
        "{tool}: the engagement pins {pinned} but the installed binary is {installed}; \
         nothing will run at a version this engagement did not pin"
    )]
    VersionMismatch {
        /// The tool whose version drifted.
        tool: &'static str,
        /// What the engagement config pinned.
        pinned: String,
        /// What was actually installed.
        installed: String,
    },

    /// One layer named some of the four inference variables but not all four.
    ///
    /// Why: the provider and the three role model ids are ONE selection — a
    /// model id only means anything in its provider's namespace. Resolving them
    /// independently pairs, say, an operator's `TRUSTY_REVIEW_PROVIDER=bedrock`
    /// with this crate's OpenRouter slugs, which is the HTTP 400 #5671 exists to
    /// remove. Guessing the missing half on someone's behalf is what produces
    /// that pairing, so a half-named selection refuses instead.
    /// What: names the layer, what it set, and what it left unset. Raised before
    /// any child is spawned, because the condition is identical for every
    /// repository in the sweep.
    /// Test: `crate::inference::inference_tests::a_partly_set_operator_environment_refuses`,
    /// `crate::run::run_tests::a_partial_operator_environment_refuses_before_any_child_runs`.
    #[error(
        "review inference is half-configured by the {layer}: {set} named, but {missing} left \
         unset. The provider and all three role models are one selection — name all four or \
         none, so a provider is never paired with another provider's model ids"
    )]
    SplitInferenceSelection {
        /// Which layer half-named the selection, for the operator to go fix.
        layer: &'static str,
        /// The variables (or config keys) that layer did set, comma-separated.
        set: String,
        /// The ones it left unset, comma-separated.
        missing: String,
    },

    /// The working directory's `tools/` area is not a real directory.
    ///
    /// Why: [`crate::workdir`] promises that `rm -rf <root>` removes everything
    /// this client wrote, which holds only while every area is a directory
    /// inside the root. A symlink planted at `tools/` before the first run
    /// redirects the install elsewhere and survives the delete. That was inert
    /// while nothing installed; #5495 is what makes it live, so #5495 checks it.
    /// What: names the path and what it is instead.
    /// Test: `crate::tools::tool_tests::a_symlinked_tools_area_is_refused`.
    #[error(
        "{path} is not a real directory (it is a {kind}), so installing there would \
         write outside the working directory; nothing was installed"
    )]
    UnsafeToolsDir {
        /// The path that failed the check.
        path: PathBuf,
        /// What it is instead — `"symlink"`, `"file"`.
        kind: &'static str,
    },

    /// The run was asked to sweep, and nothing has been selected to sweep.
    ///
    /// Why: #5555. A sweep over zero repositories that exits 0 is a report the
    /// recipient believes and that assessed nothing — the same fail-open shape
    /// the install path refuses. Absent and empty are the same state here.
    /// What: names the file repository selection is read from (#5487, #5215
    /// write it).
    /// Test: `crate::run::run_tests::an_absent_selection_is_a_refusal`.
    #[error(
        "no repositories are selected in {path}, so there is nothing to audit; \
         pick them first (`trusty-audit repos`)"
    )]
    NoRepositoriesSelected {
        /// The selection file that was absent or empty.
        path: PathBuf,
    },

    /// The selection file carries fewer entries than it declares.
    ///
    /// Why: #5555. A producer that crashes part-way through writing the file
    /// leaves syntactically valid TOML holding a PREFIX of the entries, which
    /// reads as a smaller-but-complete selection — and the sweep then reports
    /// success over a subset the operator never chose. The declared count is
    /// what makes the two distinguishable.
    /// What: names the file, what it declared, and what it holds.
    /// Test: `crate::run::run_tests::a_truncated_selection_is_refused`.
    #[error(
        "{path} declares {declared} repositories but carries {found} — it was written \
         partially. Rewrite it (write a temporary file and rename it into place); \
         nothing was audited"
    )]
    TruncatedSelection {
        /// The selection file.
        path: PathBuf,
        /// The `count` the file declared.
        declared: usize,
        /// How many `[[repositories]]` entries it actually holds.
        found: usize,
    },

    /// The run needs the pinned tools, and they are not installed.
    ///
    /// Why: #5555 runs the binaries THIS client installed and verified, never
    /// whatever is on `PATH` — an unpinned tool is the version skew #5454 cost
    /// us. There is no fallback, so this refusal is the whole behaviour.
    /// What: names each tool that is missing or that this client cannot vouch
    /// for a version of.
    /// Test: `crate::run::run_tests::a_run_without_the_pinned_tools_is_refused`.
    #[error(
        "the audit run needs the pinned tools and these are missing or unverified: {}; \
         install them first (`trusty-audit install`) — nothing will run at a version \
         this engagement did not pin",
        missing.join(", ")
    )]
    ToolsNotInstalled {
        /// The binary names that are not usable.
        missing: Vec<&'static str>,
    },

    /// Listing the repositories a credential can reach failed.
    ///
    /// Why: #5487 routes discovery through `gh`, and every way that can fail —
    /// no `gh` on PATH, no login, a whitespace `GH_TOKEN`, an org the token
    /// cannot read, a `--json` payload that did not parse — must surface as a
    /// refusal. The alternative is an empty repository list, which is
    /// indistinguishable from "this account genuinely has no repositories" and
    /// would let the recipient exclude repositories from the audit without ever
    /// being told (`crate::discover`).
    /// What: names the owner whose listing failed and keeps `trusty-common`'s
    /// structured [`trusty_common::gh::GhError`] rather than flattening it, so a
    /// front end can still tell "gh is not installed" from "you are not logged
    /// in". Boxed for the same `clippy::result_large_err` reason as
    /// [`AuditError::Install`].
    /// Test: `crate::discover::discover_tests::one_unreadable_owner_fails_the_whole_discovery`.
    #[error("cannot list the repositories {owner} can access: {source}")]
    Discovery {
        /// Whose repositories could not be listed.
        owner: String,
        /// The underlying `gh` failure, verbatim.
        #[source]
        source: Box<trusty_common::gh::GhError>,
    },

    /// A working-directory area is not a real directory, so writing there would
    /// escape the root.
    ///
    /// Why: the generalization of [`AuditError::UnsafeToolsDir`] to the second
    /// area that writes. `workdir.rs` names the debt — "repo cloning owes the
    /// same check when it lands (#5215)" — and the reason is identical: a
    /// symlink planted at `repos/` sends every clone outside the root, where it
    /// survives the delete the README promises is complete.
    /// What: names the path and what is there instead.
    /// Test: `crate::clone::clone_tests::a_symlinked_repos_area_is_refused`.
    #[error(
        "{path} is not a real directory (it is a {kind}), so writing there would go outside \
         the working directory; nothing was written"
    )]
    UnsafeArea {
        /// The path that failed the check.
        path: PathBuf,
        /// What it is instead — `"symlink"`, `"file"`.
        kind: &'static str,
    },

    /// A repository identity is not a plain `owner/name`.
    ///
    /// Why: the name becomes a path under the working directory, so `..`, an
    /// absolute path, or an embedded separator would each break the containment
    /// property `rm -rf <root>` rests on. Refused before any clone runs, so a
    /// typo in the last entry cannot leave the first ten half-acquired (#5215).
    /// What: names the rejected input verbatim.
    /// Test: `crate::clone::clone_tests::a_traversing_name_never_becomes_a_path`.
    #[error("{name} is not a repository this client will clone — it must be a plain owner/name")]
    InvalidRepoName {
        /// What was asked for.
        name: String,
    },

    /// Every requested repository failed to clone.
    ///
    /// Why: DOC-68 §14 Q2 extends DOC-67 §9's continue-on-failure policy to the
    /// clone stage — one failure is a gap, not an abort. This is the single
    /// exception it names: with no checkout at all there is nothing to audit,
    /// and a report about zero repositories is worse than a refusal (#5215).
    /// What: how many were attempted. The per-repository reasons are in the
    /// report's gaps.
    /// Test: `crate::clone::clone_tests::every_repo_failing_aborts_the_sequence`.
    #[error(
        "none of the {attempted} requested repositories could be cloned; there is nothing to audit"
    )]
    AllClonesFailed {
        /// How many clones were attempted.
        attempted: usize,
    },

    /// The sweep audited nothing, so there is no deliverable to assemble.
    ///
    /// Why: #5499's package is what the recipient sends back, and a zip holding
    /// two generated files and no report is worse than a refusal — it looks like
    /// a deliverable. The same shape as [`AuditError::AllClonesFailed`] one stage
    /// later.
    /// What: one line naming why there is nothing to package.
    /// Test: `crate::package::package_tests::a_sweep_that_audited_nothing_has_nothing_to_return`.
    #[error("{reason}")]
    NothingToPackage {
        /// Why the package was not assembled.
        reason: String,
    },

    /// A file the package would carry is a symlink.
    ///
    /// Why: the members come from `out/` and `extract/`, and following a symlink
    /// planted in either would read a file from anywhere on the recipient's
    /// machine into an archive that LEAVES their network. That is a wider
    /// consequence than [`AuditError::UnsafeArea`]'s, which only misplaces this
    /// crate's own writes (#5499).
    /// What: names the link.
    /// Test: `crate::package::package_tests::a_symlinked_member_is_refused`.
    #[error(
        "{path} is a symlink, and packaging it would send a file from outside the working \
         directory; no package was written"
    )]
    UnsafePackageEntry {
        /// The link that was refused.
        path: PathBuf,
    },

    /// A file the package would carry holds the engagement credential.
    ///
    /// Why: [`crate::config::SecretKey`] makes writing the key into a file THIS
    /// crate generates a compile error, and that is the whole of what a type can
    /// promise. The package's members are files other programs wrote, so the
    /// same invariant needs a runtime check there (#5499).
    /// What: names the file. The key itself is never quoted.
    /// Test: `crate::package::package_tests::a_member_carrying_the_credential_is_refused_and_leaves_no_zip`.
    #[error(
        "{path} contains the engagement credential, which must never leave in the return \
         package; no package was written"
    )]
    CredentialInPackage {
        /// The file that carries it.
        path: PathBuf,
    },

    /// The return package could not be read, written, or renamed into place.
    #[error("return package {path}: {source}")]
    Package {
        /// The path that failed.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// A capability this scaffold declares but does not yet implement.
    ///
    /// Why: a half-built capability must fail closed rather than silently
    /// report success. The tool-download seam this variant originally guarded
    /// is now implemented (#5495, via `trusty-installer`'s pinned entry point);
    /// the variant remains for the capabilities later milestones add.
    #[error("{capability} is not implemented yet — tracked in #{issue}")]
    NotImplemented {
        /// Human-readable capability name.
        capability: &'static str,
        /// The issue that lands it.
        issue: u32,
    },
}

#[cfg(test)]
mod error_tests {
    use super::*;

    #[test]
    fn messages_name_the_offending_path() {
        let err = AuditError::Read {
            path: PathBuf::from("/tmp/engagement/manifest.toml"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        };
        let rendered = err.to_string();
        assert!(
            rendered.contains("/tmp/engagement/manifest.toml"),
            "error should name the file: {rendered}"
        );
    }

    #[test]
    fn not_implemented_cites_the_issue_that_lands_it() {
        let err = AuditError::NotImplemented {
            capability: "tool downloads",
            issue: 5491,
        };
        assert_eq!(
            err.to_string(),
            "tool downloads is not implemented yet — tracked in #5491"
        );
    }
}
