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

use std::fmt;
use std::path::PathBuf;

/// One line of a targets file that could not be read as a target (#5978).
///
/// Why: the line NUMBER is the whole point — an operator holding a
/// thirty-line `repos.txt` needs to go straight to the three that are wrong,
/// and a message naming only the entries makes them search for each one.
/// What: the file's own name, the 1-based line number, the entry verbatim, and
/// the reason that entry specifically was refused.
/// Test: `crate::cli::targets_file::targets_file_tests::one_bad_line_refuses_the_whole_read`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadLine {
    /// Which file, e.g. `repos.txt`.
    pub file: String,
    /// 1-based line number within that file.
    pub line: usize,
    /// What was on the line, trimmed.
    pub entry: String,
    /// Why this entry is not a target.
    pub reason: String,
}

impl fmt::Display for BadLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            file,
            line,
            entry,
            reason,
        } = self;
        write!(f, "{file} line {line}: {entry} — {reason}")
    }
}

/// Every [`BadLine`] one read produced, rendered one per line.
///
/// A newtype rather than a bare `Vec` so [`AuditError::TargetsFileRefused`] can
/// interpolate the whole list into its message while the caller keeps the
/// structure to assert on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadLines(pub Vec<BadLine>);

impl fmt::Display for BadLines {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for bad in &self.0 {
            writeln!(f, "  {bad}")?;
        }
        Ok(())
    }
}

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

    /// A file the package would carry is a link to content from elsewhere.
    ///
    /// Why: the members come from `out/` and `extract/`, and packaging a link
    /// planted in either would read a file from anywhere on the recipient's
    /// machine into an archive that LEAVES their network. That is a wider
    /// consequence than [`AuditError::UnsafeArea`]'s, which only misplaces this
    /// crate's own writes (#5499).
    ///
    /// Two kinds, because they are not detectable the same way. A symlink is
    /// visible as a link in its own file type. A hardlink is not visible at all
    /// — it is an ordinary directory entry on an inode that another directory
    /// entry also names, so it reports `is_symlink() == false` and
    /// `is_file() == true`, and the only observable signal is the link count.
    /// What: names the entry and which kind it is.
    /// Test: `crate::package::package_tests::a_symlinked_member_is_refused`,
    /// `crate::package::package_tests::a_hardlinked_member_under_out_is_refused`.
    #[error(
        "{path} is a {kind}, and packaging it could send a file from outside the working \
         directory; no package was written"
    )]
    UnsafePackageEntry {
        /// The entry that was refused.
        path: PathBuf,
        /// What it is — `"symlink"` or `"hardlink"`.
        kind: &'static str,
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

    /// A repository the package includes a report for has no collection
    /// database to go with it.
    ///
    /// Why: the owner's ruling on the deliverable (2026-08-18) is "the
    /// collection db plus the detailed templated reports" — the two travel
    /// together, not one without the other. `collect_extract` used to return
    /// `Ok(())` whenever `work.path(Area::Extract)` held no `<stem>.db` for an
    /// audited repository — both when the directory itself was absent and
    /// when it existed but named nothing for this repository — so a report
    /// could ship with its database silently missing and nothing in the
    /// package said so (#5862). This refuses the whole assembly instead, the
    /// same posture as [`AuditError::CredentialInPackage`] and
    /// [`AuditError::UnsafePackageEntry`]: a package this crate is unsure
    /// about is not one it writes.
    /// What: names the repository and the database path that was expected.
    /// Test: `crate::package::package_tests::an_audited_repo_with_no_extract_database_is_refused`,
    /// `crate::package::package_tests::an_audited_repo_with_no_extract_directory_at_all_is_refused`.
    #[error(
        "{repo} was audited but has no collection database at {expected} — the delivery is the \
         database and the report together, so no package was written"
    )]
    MissingExtractDatabase {
        /// The repository whose database is missing.
        repo: String,
        /// Where the database was expected.
        expected: PathBuf,
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

    /// A generated file could not be serialized.
    ///
    /// Why: [`crate::config::generate`] re-renders a TOML table, and a render
    /// that fails must not produce a config the recipient would run against
    /// (#5825). Separate from [`AuditError::Parse`], which is about reading.
    /// What: names what was being written. Never quotes the content, because
    /// the content is the config carrying the credential.
    /// Test: `crate::config::config_tests::generating_a_config_that_would_not_load_fails_here`
    /// covers the neighbouring refusal; this arm has no reachable trigger for
    /// a `toml::Table` and exists so one cannot be swallowed.
    #[error("cannot render {what}: {source}")]
    Render {
        /// What was being written, e.g. `"engagement config"`.
        what: &'static str,
        /// The underlying serialization failure. Boxed for the same reason
        /// [`AuditError::Parse`]'s is.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// An input the inbound install package cannot be assembled without.
    ///
    /// Why: #5825's packaging step is the classic fail-open — a missing binary
    /// or an unreadable template that produces a zip anyway ships a deliverable
    /// that cannot run, and the operator finds out from the client. Every input
    /// is checked before a byte is written, and a refusal leaves nothing behind.
    /// What: names which input and where it was looked for.
    /// Test: `crate::distribute::distribute_tests::a_missing_binary_is_refused_and_leaves_no_file`,
    /// `crate::distribute::distribute_tests::a_missing_template_is_refused_and_leaves_no_file`.
    #[error("cannot build the install package: no {what} at {path}")]
    MissingPackageInput {
        /// Which input, e.g. `"taudit binary"`.
        what: &'static str,
        /// Where it was looked for.
        path: PathBuf,
    },

    /// No OpenRouter key to write into the generated engagement config.
    ///
    /// Why: the inbound package's whole purpose is that the recipient can run
    /// the audit without supplying anything of ours, and a config with a blank
    /// key produces a package that installs, launches, and then fails at the
    /// first inference call (#5825). Refusing at packaging time is where the
    /// operator can still fix it.
    /// What: names the environment variable to set and the template that could
    /// have carried one instead. The key itself is never quoted, and neither is
    /// a blank one — there is nothing to quote.
    /// Test: `crate::distribute::distribute_tests::a_blank_credential_is_refused_and_leaves_no_file`.
    #[error(
        "cannot build the install package: no OpenRouter key. Set {env}, or put one in the \
         template config at {template}"
    )]
    MissingCredential {
        /// The environment variable that supplies it.
        env: &'static str,
        /// The template config that could carry one instead.
        template: PathBuf,
    },

    /// The engagement config carries a key, but it is blank.
    ///
    /// Why: a present-but-empty `openrouter_key` used to sail through. The
    /// sweep started, [`crate::inference::inference_env`] read the blank key as
    /// "select nothing" and returned no variables, and the `tga audit` child
    /// ran without its `TRUSTY_REVIEW_*` selection — so `trusty-review` fell
    /// back to its Bedrock default. The operator found out hours later, at the
    /// report stage, as a missing-AWS-credentials failure; the worse outcome is
    /// the one where it works and bills a provider nobody chose. Refusing
    /// before the sweep starts is the whole point (#5868).
    /// What: names the file and both ways to put a key in play. Nothing is
    /// quoted — a blank key has nothing to quote.
    /// Test: `crate::session::session_tests::a_blank_configured_key_refuses_before_the_sweep`.
    #[error(
        "the OpenRouter key in {config} is blank. The audit's report stage cannot run without \
         one: set {env} in the environment, or set `openrouter_key` in that file"
    )]
    BlankCredential {
        /// The engagement config carrying the blank key.
        config: PathBuf,
        /// The environment variable that supplies one instead.
        env: &'static str,
    },

    /// Nothing configured a credential, and there is no terminal to ask on.
    ///
    /// Why: the install path this crate is heading for is `curl … | sh`, where
    /// stdin is the pipe carrying the script text. A run with no controlling
    /// terminal must therefore say what to do and stop — hanging on a read, or
    /// treating the next line of that pipe as a credential, are the two
    /// failures this refusal exists to rule out (#5868).
    /// What: names both ways to supply the key. The key itself is never
    /// quoted, because there is none to quote.
    /// Test: `crate::cli::credential::credential_tests::without_a_terminal_the_refusal_names_both_sources`.
    #[error(
        "no OpenRouter key is configured, and there is no terminal to ask on. Either set {env} \
         in the environment, or set `openrouter_key` in {config}"
    )]
    NoCredentialSource {
        /// The environment variable that supplies it.
        env: &'static str,
        /// The engagement config that could carry one instead.
        config: PathBuf,
    },

    /// The operator was asked and did not produce a usable key.
    ///
    /// Why: the prompt re-asks a mistyped or blank entry rather than exiting on
    /// the first mistake, but it cannot re-ask forever — a `/dev/tty` that
    /// opens and then immediately reports end-of-file would spin (#5868).
    /// What: names how many attempts were made. Never quotes what was typed,
    /// including the entry that failed to match.
    /// Test: `crate::cli::credential::credential_tests::a_prompt_that_never_agrees_gives_up`.
    #[error("no usable OpenRouter key was entered after {attempts} attempts")]
    CredentialNotEntered {
        /// How many times the key was asked for.
        attempts: usize,
    },

    /// The terminal could not be read from or written to.
    ///
    /// Why: distinguished from [`AuditError::CredentialNotEntered`] because the
    /// operator did nothing wrong and re-running will not help until whatever
    /// broke the terminal is fixed (#5868).
    /// What: carries the underlying I/O failure. `std::io::Error` renders the
    /// OS message only, so nothing that was typed can reach it.
    /// Test: `crate::cli::credential::credential_tests::a_terminal_that_fails_is_reported_without_the_entry`.
    #[error("cannot read the OpenRouter key from the terminal: {source}")]
    CredentialPromptFailed {
        /// What the terminal read or write failed with.
        #[source]
        source: std::io::Error,
    },

    /// The terminal broke while the guided flow was collecting audit targets.
    ///
    /// Why: separate from [`AuditError::CredentialPromptFailed`] because the two
    /// name different prompts, and an operator whose terminal died mid-
    /// registration needs to know that what they already entered is on disk
    /// (#5885). Every entry is persisted as it lands, so re-running resumes.
    /// What: carries the underlying I/O failure. A refused TARGET is not this —
    /// that is reported in the loop and the loop asks again.
    /// Test: `crate::cli::registration::registration_tests::a_terminal_that_dies_keeps_what_was_already_registered`.
    #[error("cannot read the audit targets from the terminal: {source}")]
    RegistrationPromptFailed {
        /// What the terminal read or write failed with.
        #[source]
        source: std::io::Error,
    },

    /// A `repos.txt` or `boards.txt` line is not a target, so none was taken.
    ///
    /// Why: #5978. Skipping the bad lines and registering the rest produces an
    /// audit that covers fewer repositories than the file lists and reports
    /// success over the absent ones. Every bad line is named at once so one run
    /// fixes them all, and the message says the OpenRouter key is already saved
    /// because the operator's next thought is whether re-running costs them the
    /// key prompt again.
    /// What: carries every refused line, from both files, with its number and
    /// its own reason. The entry is quoted; a targets file holds no credential.
    /// Test: `crate::cli::targets_file::targets_file_tests::one_bad_line_refuses_the_whole_read`.
    #[error(
        "these lines are not audit targets:\n{bad}\nNothing was registered. Fix them and run \
         `trusty-audit` again — your OpenRouter key is saved, so you will not be asked for it \
         again."
    )]
    TargetsFileRefused {
        /// Every line that could not be read, in file order.
        bad: BadLines,
    },

    /// The install package's destination is already taken.
    ///
    /// Why: #5825 — an operator regenerating a package must not destroy the one
    /// they already sent. Versioning the name instead would leave two files a
    /// hand could pick the wrong one of; a refusal puts the decision at the
    /// moment the mistake would happen, and `rm` is the explicit act that says
    /// the old package no longer matters.
    /// What: names the file and what to do about it.
    /// Test: `crate::distribute::distribute_tests::an_existing_package_is_never_overwritten`.
    #[error(
        "{path} already exists. This client never overwrites a package that may already have \
         been sent — remove it, or choose another --out"
    )]
    PackageExists {
        /// The destination that is already taken.
        path: PathBuf,
    },

    /// The install package could not be read, written, or renamed into place.
    ///
    /// Deliberately a different variant from [`AuditError::Package`]: the two
    /// travel in opposite directions and their messages must not be confusable
    /// in an operator's terminal (#5825).
    #[error("install package {path}: {source}")]
    Distribute {
        /// The path that failed.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// A registration spec is not a target this client will register.
    ///
    /// Why: #5822. `taudit add` writes the spec into a file the sweep later
    /// reads, and a repository identity becomes a path under the working
    /// directory, so the same containment argument as
    /// [`AuditError::InvalidRepoName`] applies — `crate::registry` reuses
    /// `crate::clone`'s charset check rather than deciding a second one.
    /// What: quotes the rejected input and names the shape that was expected.
    /// Test: `crate::registry::registry_tests::a_spec_that_is_neither_shape_is_refused`.
    #[error("{spec} is not a target this client will register — {expected}")]
    InvalidTarget {
        /// What was asked for, verbatim.
        spec: String,
        /// The shape that was expected, e.g. `"expected owner/name"`.
        expected: &'static str,
    },

    /// A board was registered and the engagement config carries no credential
    /// for its provider.
    ///
    /// Why: registration VALIDATES before it persists (#5822), and a validation
    /// that cannot even be attempted must say what to go and set — the
    /// recipient is not the author of this config. Refusing here also keeps the
    /// registry honest: nothing is persisted that was never checked.
    /// What: names the provider and the exact config field.
    /// Test: `crate::registry::registry_tests::a_board_without_a_credential_names_the_config_field`.
    #[error(
        "no {provider} credential in the engagement config, so {key} cannot be checked; \
         set `{field}` in the engagement config and add it again — nothing was registered"
    )]
    BoardCredentialMissing {
        /// `"jira"` or `"linear"`.
        provider: &'static str,
        /// The board that could not be checked.
        key: String,
        /// The config field to set, e.g. `"boards.jira"`.
        field: &'static str,
    },

    /// A repository was registered and the recipient's `gh` credential cannot
    /// read it.
    ///
    /// Why: #5822's whole point — a target that cannot be reached is rejected at
    /// registration, not discovered an hour into a sweep. The `gh` failure is
    /// kept structured for the same reason [`AuditError::Discovery`] keeps it:
    /// "gh is not installed" and "you cannot see that repository" call for
    /// different actions.
    /// What: names the repository; the reason is `gh`'s own.
    /// Test: `crate::validate::validate_tests::a_gh_that_cannot_answer_refuses_the_repository`.
    #[error("cannot read {name_with_owner} with your GitHub credential: {source}")]
    RepoUnreachable {
        /// The repository that was refused.
        name_with_owner: String,
        /// The underlying `gh` failure, verbatim. Boxed for the same
        /// `clippy::result_large_err` reason as [`AuditError::Install`].
        #[source]
        source: Box<trusty_common::gh::GhError>,
    },

    /// A board was registered and the configured credential cannot read it.
    ///
    /// Why: the board half of [`AuditError::RepoUnreachable`] (#5822).
    /// What: names the provider and the board. `reason` is built from the HTTP
    /// status and, for a GraphQL refusal, the provider's own message — never
    /// from the request, so the credential cannot reach this string. That is
    /// asserted directly rather than argued.
    /// Test: `crate::validate::validate_tests::a_provider_message_is_scrubbed_of_the_credential`.
    #[error("cannot read {provider} {key} with the configured credential: {reason}")]
    BoardUnreachable {
        /// `"jira"` or `"linear"`.
        provider: &'static str,
        /// The board that was refused.
        key: String,
        /// Why, in one line. Never carries a credential.
        reason: String,
    },

    /// The target registry carries fewer entries than it declares.
    ///
    /// The registry's half of [`AuditError::TruncatedSelection`], for the same
    /// reason and detected the same way — see `crate::registry` (#5822).
    #[error(
        "{path} declares {declared} targets but carries {found} — it was written partially. \
         Register them again; nothing was changed"
    )]
    TruncatedRegistry {
        /// The registry file.
        path: PathBuf,
        /// The `count` the file declared.
        declared: usize,
        /// How many `[[targets]]` entries it actually holds.
        found: usize,
    },

    /// A target was named, and there is no engagement to declare it in.
    ///
    /// Why: #5979 makes `engagement.toml` the file that says what an engagement
    /// covers, so there is nowhere to record a target until one exists. The
    /// alternative — writing the working copy and calling it registered — is the
    /// split this change removes, and it would report success over a file the
    /// sweep no longer treats as authoritative.
    ///
    /// A cold-start launch writes the config before it asks for a target
    /// (#5970), so reaching this means `taudit add` was run directly in a
    /// directory that is not an engagement yet.
    /// What: names the path that was looked for and the command that creates it.
    /// Nothing was written.
    /// Test: `crate::registry::registry_tests::registering_without_a_config_is_refused`.
    #[error(
        "there is no engagement config at {path}, so there is nothing to register a target in. \
         Run `trusty-audit` in this directory to set one up first; nothing was registered"
    )]
    NoEngagementConfig {
        /// Where the config was expected.
        path: PathBuf,
    },

    /// The registry's read-modify-write could not be serialised.
    ///
    /// Why: #5822 — registering and removing a target both load
    /// `state/audit-targets.toml`, mutate it and save it back. Two of those
    /// running at once against one working directory each load the same
    /// snapshot, and the later save discards the earlier one's target while both
    /// report success. `trusty_common::file_lock::with_exclusive_lock` closes
    /// that, and it never fails open — so a lock that cannot be taken has to be
    /// a refusal here rather than an unserialised write.
    /// What: names the registry the lock guards. `source` is the lock failure,
    /// or the panic that escaped the critical section.
    /// Test: `crate::registry::registry_tests::concurrent_registrations_keep_every_target`.
    #[error("could not update {path} under its lock: {source}. Nothing was changed")]
    RegistryLock {
        /// The registry file whose lock could not be held.
        path: PathBuf,
        /// Why the critical section could not be entered or completed.
        #[source]
        source: std::io::Error,
    },

    /// The one-shot audit stopped, and this is the phase it stopped in.
    ///
    /// Why: #5824 chains four fallible phases, and an operator who sees it stop
    /// must know which one. Reporting every phase's failure as "the audit
    /// failed" makes them re-derive the stage before they can act — "cannot read
    /// the registry" and "`tga audit` exited with code 2" have nothing in common
    /// as next steps.
    /// What: the phase, plus the underlying refusal, which names the target and
    /// the reason. `source` is boxed because nesting one `AuditError` inside
    /// another unboxed would grow every `Result` in the crate past
    /// `clippy::result_large_err`.
    /// Test: `crate::chain::chain_tests::a_sweep_that_audited_nothing_stops_before_packaging`.
    #[error("the audit stopped in the {phase} phase — {source}")]
    ChainStopped {
        /// Which link of the chain refused.
        phase: crate::chain::Phase,
        /// What it refused with.
        #[source]
        source: Box<AuditError>,
    },

    /// The sweep ran and audited no repository at all.
    ///
    /// Why: #5824's fail-open guard. The chain continues past a repository that
    /// failed, because five audits are worth having when the sixth did not work.
    /// A sweep in which EVERY repository failed is not that case: packaging it
    /// would hand the recipient a zip of two generated files that looks like a
    /// finished engagement. [`AuditError::NothingToPackage`] is the same refusal
    /// one stage later, from `crate::package`; this one exists so the chain
    /// attributes it to the phase that actually failed.
    /// What: how many repositories were attempted. The per-repository reasons
    /// are in `state/run-progress.toml` and in each repository's log.
    /// Test: `crate::chain::chain_tests::a_sweep_that_audited_nothing_stops_before_packaging`.
    #[error(
        "no repository was audited ({attempted} attempted) — nothing was packaged; see the \
         per-repository failures above, and `trusty-audit package` if you want the partial \
         output anyway"
    )]
    NothingAudited {
        /// How many repositories the sweep attempted.
        attempted: usize,
    },

    /// The chain was asked to audit an engagement that targets nothing.
    ///
    /// Why: #5824. `NoRepositoriesSelected` names a file the operator has never
    /// heard of and does not write, so on the one-shot path it is the wrong
    /// message — the remedy is to register a target, not to go and edit
    /// `state/selected-repos.toml`.
    /// What: names the working directory and the command that fixes it.
    /// Test: `crate::chain::chain_tests::an_engagement_with_nothing_to_audit_stops_in_materialize`.
    #[error(
        "nothing to audit in {root} — no repository is registered and no earlier clone left a \
         selection; register one with `trusty-audit add repo <owner>/<name>`"
    )]
    NothingRegistered {
        /// The working directory that targets nothing.
        root: PathBuf,
    },

    /// The key was entered, and the engagement config it belongs in could not be
    /// written.
    ///
    /// Why: the fail-open shape this variant exists to rule out (#5970). A cold
    /// start that carried on here would hold the recipient's key in memory for
    /// one process, run an hours-long sweep on it, and leave nothing behind — so
    /// the next launch asks again and no engagement was ever set up. The launch
    /// stops instead, and says which file it could not write.
    /// What: names the path and keeps the underlying [`AuditError::WorkDir`]
    /// failure. Never quotes the key, which is why the message can say what it
    /// says about it.
    /// Test: `crate::cli::bootstrap::bootstrap_tests::a_config_that_cannot_be_written_stops_the_launch`.
    #[error(
        "the engagement config at {path} could not be written, so nothing was set up and \
         the key you entered was NOT saved; fix that path and run `trusty-audit` again: {source}"
    )]
    EngagementNotCreated {
        /// The config that could not be written.
        path: PathBuf,
        /// What the write failed with. Boxed — `AuditError` inside itself.
        #[source]
        source: Box<AuditError>,
    },

    /// A cold start could not learn which version of a tool to pin.
    ///
    /// Why: #5970's cold start records the version of each tool it resolved into
    /// `engagement.toml`, and that file is the pin from then on. A resolution
    /// that failed must not be papered over with a guess — a guessed version is
    /// the #5454 version skew with a config file vouching for it. So the launch
    /// stops before anything is written.
    /// What: names the tool whose release list could not be read.
    /// Test: `crate::tools::tool_tests::an_unresolvable_pin_names_the_tool`.
    #[error(
        "cannot determine which version of {tool} to pin — its release list could not be \
         read; no engagement was created: {source}"
    )]
    PinsUnresolved {
        /// The crate whose latest release could not be resolved.
        tool: &'static str,
        /// The underlying lookup failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// A pinned tool cannot be installed on this machine.
    ///
    /// Why: the preflight #5970 added exists so a recipient learns this before
    /// committing to a multi-tool download, and learns WHICH tool. Reported as a
    /// refusal rather than a warning for the same reason every other install-side
    /// variant is: a set that is missing one tool cannot run a sweep, and a sweep
    /// over three of four tools is the degraded success #5454 cost us.
    /// What: names the tool and keeps `trusty-installer`'s structured
    /// [`trusty_installer::download::pinned::PinnedError`], which already states
    /// whether the pin is unpublished, the host unsupported, or the release list
    /// unreachable — three problems with three different remedies. Boxed for the
    /// `clippy::result_large_err` reason [`AuditError::Install`] documents.
    /// Test: `crate::tools::tool_tests::a_tool_that_cannot_be_installed_is_refused_by_name`.
    #[error("{tool} cannot be installed on this machine: {source}")]
    ToolNotInstallable {
        /// The crate that cannot be installed.
        tool: &'static str,
        /// Why, verbatim from the installer.
        #[source]
        source: Box<trusty_installer::download::pinned::PinnedError>,
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
