//! The pinned tools this client runs: which ones, at what versions, and the
//! call into `trusty-installer` that fetches them.
//!
//! Why: the owner's framing is that the auditor client is an installer runner —
//! it fetches `tga`, `trusty-analyze` and `trusty-review` at pinned versions and
//! then drives them. Downloading is `trusty-installer`'s domain, and CLAUDE.md's
//! "Common entry point, clean domain demarcation" rule makes a second download
//! implementation a defect rather than a shortcut. So this module decides WHICH
//! tools at WHICH versions, and hands that to
//! [`trusty_installer::download::pinned::install_pinned_set`]. There is no
//! download, checksum, extraction, or archive code here, and there must never
//! be: `install_pinned_set` is the entry point, `cargo install` is unreachable
//! from it, and adding a fallback here would reintroduce exactly the
//! degraded-success path #5491 exists to remove.
//!
//! What: [`RequiredTool`], the three-tool set; [`plan`], which turns the
//! engagement config's [`ToolPins`] into the installer's pin values;
//! [`install`], which stages and places all three or none; [`verify`], the
//! client's own check that what came back is what was pinned; and the version
//! record under `state/`, which is what lets the deliverable state the exact
//! triple that produced it (#5495).
//!
//! ## Fail-closed, everywhere
//!
//! There is no branch in this module that proceeds after a failure. A version
//! that does not match the pin, a checksum mismatch, a truncated download, a
//! network error mid-stream, a `tools/` directory that is really a symlink —
//! each returns an [`AuditError`] and installs nothing. The reason is #5454:
//! a new `tga` paired with an old `trusty-review` produced a deterministic
//! report and exited 0. Every silent degrade on this path ends the same way, in
//! a report the recipient believes and cannot reproduce.
//!
//! ## Known v1 risk: a network that blocks binary downloads
//!
//! A recipient behind an egress proxy that blocks arbitrary binary downloads
//! fails HERE — in the first thirty seconds, with
//! [`AuditError::Install`] naming the URL that could not be reached — rather
//! than an hour into an audit. That is the good version of this failure, and it
//! is as far as v1 goes: nothing here retries through a proxy, and there is no
//! bundled-binary fallback variant. Whether one should exist is deferred
//! (#5495), not solved. The recipient's remedy today is to allow the
//! `github.com` release-asset host, or to ask for a package built for their
//! network.
//!
//! Test: `super::tool_tests`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use trusty_installer::download::pinned::{PinnedInstall, PinnedTool};

use crate::config::{ToolPin, ToolPins};
use crate::error::AuditError;
use crate::progress::{Operation, Progress, UnitOutcome};
use crate::workdir::{Area, WorkDir};

/// File under `state/` recording the exact versions that were installed.
pub const VERSION_RECORD_FILE: &str = "tool-versions.toml";

/// A tool the audit run needs on disk before it can start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RequiredTool {
    /// The audit sweep itself.
    Tga,
    /// The search daemon the rest of the stack stands on (#5670).
    ///
    /// Why it is a pinned tool rather than a machine prerequisite: `tga audit`
    /// starts this daemon itself, and on a recipient's clean machine the only
    /// copy it can start is the one this client installed. Without it here,
    /// `TRUSTY_SEARCH_BIN` would have nothing to name and the guard would fall
    /// through to a `PATH` lookup — the one thing an isolated run must not do.
    TrustySearch,
    /// Static analysis feeding the scorecard.
    TrustyAnalyze,
    /// Report rendering.
    TrustyReview,
}

impl RequiredTool {
    /// Every tool the run needs (#5495, #5670).
    ///
    /// The order is the prerequisite chain: trusty-search must exist before
    /// `trusty-analyze` can boot against it, and both before `tga` sweeps.
    pub const ALL: [RequiredTool; 4] = [
        RequiredTool::Tga,
        RequiredTool::TrustySearch,
        RequiredTool::TrustyAnalyze,
        RequiredTool::TrustyReview,
    ];

    /// The binary name as it lands in the working directory's `tools/` area.
    pub fn binary_name(self) -> &'static str {
        match self {
            RequiredTool::Tga => "tga",
            RequiredTool::TrustySearch => "trusty-search",
            RequiredTool::TrustyAnalyze => "trusty-analyze",
            RequiredTool::TrustyReview => "trusty-review",
        }
    }

    /// The crate `trusty-installer` fetches to produce that binary.
    pub fn crate_name(self) -> &'static str {
        match self {
            RequiredTool::Tga => "tga",
            RequiredTool::TrustySearch => "trusty-search",
            RequiredTool::TrustyAnalyze => "trusty-analyze",
            RequiredTool::TrustyReview => "trusty-review",
        }
    }

    /// Where this tool's binary belongs under the working directory.
    pub fn path_in(self, work: &WorkDir) -> PathBuf {
        work.path(Area::Tools).join(self.binary_name())
    }

    /// This tool's pin, from the engagement config.
    ///
    /// Why: an exhaustive match, so adding a fifth tool to [`RequiredTool`]
    /// fails to compile until [`ToolPins`] gains a field for it. A lookup by
    /// string key would instead resolve to "absent" at runtime, and absent is
    /// how an unpinned tool gets fetched at whatever version is current.
    /// What: the field of `pins` corresponding to `self`.
    /// Test: `super::tool_tests::the_plan_pins_every_tool_from_the_config`.
    pub fn pin_in(self, pins: &ToolPins) -> &ToolPin {
        match self {
            RequiredTool::Tga => &pins.tga,
            RequiredTool::TrustySearch => &pins.trusty_search,
            RequiredTool::TrustyAnalyze => &pins.trusty_analyze,
            RequiredTool::TrustyReview => &pins.trusty_review,
        }
    }
}

/// Whether a required tool is on disk, and at which verified version.
///
/// Why: the guided flow's first honest answer to "can this run?" is which tools
/// are missing. Once one is present, the next honest question is which version
/// it is — and the only trustworthy answer is the one this client recorded when
/// it installed and verified the binary itself. `version: None` on an installed
/// tool therefore means something specific: a file is there that this client did
/// not put there at a known pin, so nothing may claim a version for it.
/// What: the tool, the path, whether a file is there, and the recorded version.
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
    /// The version this client recorded on installing it, if it installed it.
    pub version: Option<String>,
}

/// Whether a pinned tool is here, and if not whether it could be (#5970).
///
/// Why: [`ToolStatus`] answers "is it on disk", which is the whole question once
/// an engagement is set up. A cold start has a second one — "and if not, can this
/// machine get it" — and an operator about to wait several minutes for four
/// downloads is owed the answer first.
/// What: the tool, the version pinned for it, whether it is already satisfied,
/// and the installer's own reason when it cannot be installed. `installed: true`
/// always carries `problem: None`: nothing asked.
/// Test: `super::tool_tests::preflight_asks_nothing_about_a_satisfied_set`.
#[derive(Debug)]
#[non_exhaustive]
pub struct ToolAvailability {
    /// The tool checked.
    pub tool: RequiredTool,
    /// The version this engagement pins for it.
    pub version: String,
    /// Whether it is already on disk at that version, verified.
    pub installed: bool,
    /// Why it cannot be installed here, or `None` when it can.
    pub problem: Option<Box<trusty_installer::download::pinned::PinnedError>>,
}

/// One tool this client installed and verified, at an exact version.
///
/// Why: this is what the deliverable records so a reader knows which triple
/// produced the report (#5495, closure condition 3). It is written to `state/`
/// on a successful install and read back by anything assembling the package.
/// What: the crate name, the verified version, and where the binary landed.
/// Test: `super::tool_tests::an_install_record_round_trips`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct InstalledTool {
    /// The crate that was installed.
    pub crate_name: String,
    /// The exact version, equal to the pin and observed from the binary.
    pub version: String,
    /// Where the binary landed.
    pub binary: PathBuf,
}

/// The boundary crossing from `trusty-installer`'s result to ours.
///
/// Why: `PinnedInstall` is `#[non_exhaustive]`, so it cannot be constructed
/// outside its crate — which would leave [`verify`] untestable without a
/// network if it took that type. Converting first keeps every check in `verify`
/// provable offline. The conversion itself is a total field copy with no
/// decision in it; the decisions are all in `verify`.
/// What: crate name, version and binary path, verbatim.
/// Test: exercised on the happy path by `tests/install_fails_closed.rs`.
impl From<&PinnedInstall> for InstalledTool {
    fn from(install: &PinnedInstall) -> Self {
        Self {
            crate_name: install.crate_name.clone(),
            version: install.version.clone(),
            binary: install.binary_path.clone(),
        }
    }
}

/// The `state/tool-versions.toml` document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct VersionRecord {
    #[serde(default)]
    tools: Vec<InstalledTool>,
}

/// Probe the working directory for every required tool.
///
/// Why: pure enough to test with a temp directory, and the only thing that can
/// be truthfully reported about tooling before a run.
/// What: one [`ToolStatus`] per [`RequiredTool::ALL`] entry, in that order,
/// joined against the version record. Checking a file's existence is all this
/// does — re-verifying a binary's checksum is `trusty-installer`'s job, not a
/// second implementation here.
/// Test: `super::tool_tests::status_reports_a_tool_that_is_on_disk`.
///
/// # Errors
///
/// [`AuditError::Read`] or [`AuditError::Parse`] when a version record exists
/// but cannot be read. A malformed record is an error rather than a shrug: it
/// would otherwise read as "installed, version unknown", which is the state a
/// tampered-with record wants to look like.
pub fn status(work: &WorkDir) -> Result<Vec<ToolStatus>, AuditError> {
    let recorded = read_record(work)?;
    Ok(RequiredTool::ALL
        .iter()
        .map(|tool| {
            let path = tool.path_in(work);
            ToolStatus {
                tool: *tool,
                installed: path.is_file(),
                version: recorded
                    .iter()
                    .find(|r| r.crate_name == tool.crate_name())
                    .map(|r| r.version.clone()),
                path,
            }
        })
        .collect())
}

/// Turn the engagement's pins into the installer's pin values.
///
/// Why: pure, so the mapping is provable without a network — and the mapping is
/// where a mistake would be invisible at runtime. A wrong `binary` name means
/// the installer probes the wrong executable for the version proof; a dropped
/// `sha256` silently downgrades a byte-pinned tool to a version-pinned one.
/// What: one [`PinnedTool`] per [`RequiredTool::ALL`] entry, in that order,
/// carrying the crate name, the exact version, the binary that must prove it,
/// and the caller-supplied digest when the config pinned one.
/// Test: `super::tool_tests::the_plan_pins_every_tool_from_the_config`,
/// `super::tool_tests::a_digest_pin_reaches_the_installer`.
pub fn plan(pins: &ToolPins) -> Vec<PinnedTool> {
    RequiredTool::ALL
        .iter()
        .map(|tool| {
            let pin = tool.pin_in(pins);
            let pinned =
                PinnedTool::new(tool.crate_name(), pin.version()).with_binary(tool.binary_name());
            match pin.sha256() {
                Some(digest) => pinned.with_sha256(digest),
                None => pinned,
            }
        })
        .collect()
}

/// Check that what was installed is what was pinned.
///
/// Why: `install_pinned_set` already proves each binary reports its pin before
/// placing anything, so this is the client's own second look — the belt to that
/// braces. It costs nothing and it is the one check that would catch the
/// installer's postcondition failing. The defect it guards against is #5454's:
/// a mismatched triple that runs to completion and exits 0.
///
/// # Postconditions
/// On `Ok`, the returned vector has one entry per [`RequiredTool::ALL`] tool, in
/// that order, and each entry's version equals the pin. On `Err`, nothing is
/// claimed about any tool — the caller must not install, record, or run.
///
/// What: matches `reported` to pins by crate name rather than by position, so a
/// reordered result is caught as a mismatch instead of silently misattributed.
/// Test: `super::tool_tests::a_version_that_drifts_from_the_pin_is_refused`,
/// `super::tool_tests::a_missing_install_is_refused`.
///
/// # Errors
///
/// [`AuditError::VersionMismatch`] when a tool is absent from `reported` or came
/// back at a version other than the one pinned.
pub fn verify(
    pins: &ToolPins,
    reported: &[InstalledTool],
) -> Result<Vec<InstalledTool>, AuditError> {
    RequiredTool::ALL
        .iter()
        .map(|tool| {
            let pinned = tool.pin_in(pins).version();
            match reported.iter().find(|i| i.crate_name == tool.crate_name()) {
                Some(install) if install.version == pinned => Ok(install.clone()),
                Some(install) => Err(AuditError::VersionMismatch {
                    tool: tool.crate_name(),
                    pinned: pinned.to_owned(),
                    installed: install.version.clone(),
                }),
                None => Err(AuditError::VersionMismatch {
                    tool: tool.crate_name(),
                    pinned: pinned.to_owned(),
                    installed: "nothing — the installer reported no entry for it".to_owned(),
                }),
            }
        })
        .collect()
}

/// Download, verify and place all three pinned tools, or none of them.
///
/// Why: #5495. The client consumes `trusty-installer`'s pinned, fail-closed
/// entry point rather than owning a second download implementation, and it
/// records the resulting triple so the deliverable can state which versions
/// produced it.
///
/// # Preconditions
/// `pins` names an exact version per tool. The type makes that unavoidable —
/// [`ToolPins`]' three fields are required, so a partly-pinned engagement config
/// never parses.
///
/// # Postconditions
/// On `Ok`, `work`'s `tools/` area holds all three binaries, each observed
/// reporting its pinned version, and `state/`[`VERSION_RECORD_FILE`] records the
/// triple. On `Err`, nothing ran and nothing may be claimed about any tool;
/// whether the install directory is clean is stated by the wrapped
/// [`trusty_installer::download::pinned::PinnedError`], which is the only type
/// that knows.
///
/// What: creates the working directory, refuses a `tools/` area that is not a
/// real directory, hands the plan to `install_pinned_set`, re-checks the
/// versions with [`verify`], then writes the record. The record is written LAST,
/// so a failure anywhere leaves no record claiming an install that did not
/// happen.
/// Test: `super::tool_tests` covers every step that does not need a network;
/// `tests/install_fails_closed.rs` drives the whole path through the CLI.
///
/// # Errors
///
/// [`AuditError::UnsafeToolsDir`] when `tools/` is not a real directory,
/// [`AuditError::Install`] for any failure inside `trusty-installer` — an
/// unpublished version, an unreachable host, a checksum mismatch, a binary that
/// reports the wrong version — [`AuditError::VersionMismatch`] when the returned
/// set does not match the pins, and [`AuditError::WorkDir`] when the record
/// cannot be written.
pub async fn install(
    work: &WorkDir,
    pins: &ToolPins,
    progress: &Progress,
) -> Result<Vec<InstalledTool>, AuditError> {
    work.create()?;
    let install_dir = work.path(Area::Tools);
    // #5495: workdir.rs left this check to this issue — a symlinked `tools/`
    // was inert while nothing installed, and installing is what makes it live.
    ensure_real_directory(&install_dir)?;

    let plan = plan(pins);
    // #5823: the download is a single all-or-none call into `trusty-installer`,
    // which reports nothing per tool — so this is honest about what it knows.
    // The operation start is what puts a spinner on screen for the wait; the
    // per-tool lines come after, from the set that actually landed. Per-tool
    // progress DURING the download needs a hook in `install_pinned_set` that
    // does not exist, and inventing one here would mean a second downloader.
    progress.operation_started(Operation::InstallTools, plan.len());
    let installs = trusty_installer::download::pinned::install_pinned_set(
        &trusty_installer::download::http_client(),
        &plan,
        &install_dir,
    )
    .await
    .map_err(|source| AuditError::Install {
        source: Box::new(source),
    })?;

    let reported: Vec<InstalledTool> = installs.iter().map(InstalledTool::from).collect();
    let installed = verify(pins, &reported)?;
    for (index, tool) in installed.iter().enumerate() {
        progress.unit_started(
            Operation::InstallTools,
            tool.crate_name.as_str(),
            index + 1,
            installed.len(),
        );
        progress.unit_finished(
            Operation::InstallTools,
            tool.crate_name.as_str(),
            UnitOutcome::Succeeded,
        );
    }
    progress.operation_finished(Operation::InstallTools, installed.len(), plan.len());
    write_record(work, &installed)?;
    Ok(installed)
}

/// The tools this engagement's pins are not already satisfied by.
///
/// Why: the idempotence test for auto-install (#5797). "Already installed" is
/// not a question about the file alone — `crate::run::pinned_binaries` refuses a
/// sweep on three conditions, and anything that decides whether to download must
/// use the same three or the two disagree. A predicate that asked only whether
/// the file exists would skip the download, and then the sweep would refuse over
/// the binary it just declined to replace.
///
/// The three conditions, and what each one being false means:
/// - the file is absent — `MISSING` in the `tools` report;
/// - a file is there but the version record does not name it — `UNVERIFIED`,
///   a binary this client did not place, whose version nothing may claim. It is
///   reinstalled rather than kept: an unverifiable copy is exactly the #5454
///   version-skew input, and `tools/` is this client's own scratch area under
///   the work-dir root, so replacing it costs the recipient nothing;
/// - the recorded version is not the pin — the config moved after the install.
///
/// What: one [`RequiredTool`] per unsatisfied tool, in [`RequiredTool::ALL`]
/// order. Empty means a run may proceed and nothing needs downloading.
/// Test: `super::tool_tests::a_verified_set_at_the_pin_is_satisfied`,
/// `super::tool_tests::an_unverified_binary_is_unsatisfied`,
/// `super::tool_tests::a_recorded_version_behind_the_pin_is_unsatisfied`.
///
/// # Errors
///
/// Whatever [`status`] fails with — an unreadable or malformed version record.
pub fn unsatisfied(work: &WorkDir, pins: &ToolPins) -> Result<Vec<RequiredTool>, AuditError> {
    Ok(status(work)?
        .into_iter()
        .filter(|s| {
            let pinned = s.tool.pin_in(pins).version();
            !(s.installed && s.version.as_deref() == Some(pinned))
        })
        .map(|s| s.tool)
        .collect())
}

/// Install the pinned set only if it is not already on disk at those versions.
///
/// Why: #5797 — the owner's "we should be installing these automatically". The
/// separate `install` step was the only thing standing between a recipient and a
/// run, and asking them to read a status table and then retype a command is work
/// this client can do itself.
///
/// Two properties this must not trade away, and does not:
/// - **Fail-closed is unchanged.** This is a call to [`install`], which is
///   all-or-none, so an auto-install that cannot resolve every pin installs
///   nothing and returns [`AuditError::Install`]. The caller propagates it. What
///   #5454 forbids is a mismatched set running to completion and exiting 0, and
///   that is a property of the SET, not of who asked for the install.
/// - **It does not download on every invocation.** [`unsatisfied`] is a
///   filesystem read of `tools/` and `state/`, and an already-satisfied set
///   returns `Ok(None)` without constructing an HTTP client.
///
/// What: `Ok(None)` when every pin was already satisfied and nothing ran;
/// `Ok(Some(installed))` naming the set this call placed.
/// Test: `super::tool_tests::ensure_is_a_no_op_once_the_set_is_satisfied`,
/// `super::tool_tests::ensure_over_an_unverified_binary_reaches_the_installer`.
///
/// # Errors
///
/// Everything [`install`] returns, plus [`status`]'s read and parse failures.
pub async fn ensure(
    work: &WorkDir,
    pins: &ToolPins,
    progress: &Progress,
) -> Result<Option<Vec<InstalledTool>>, AuditError> {
    if unsatisfied(work, pins)?.is_empty() {
        return Ok(None);
    }
    install(work, pins, progress).await.map(Some)
}

/// Resolve the latest published stable release of each required tool (#5970).
///
/// Why: a cold start has no `engagement.toml`, so it has no pins — and there is
/// nowhere else in this crate to get them from. The two candidates were a
/// compiled-in default set and this. A compiled-in set is a constant nothing in
/// this workspace bumps: `trusty-audit` releases rarely and the four tools
/// release often, so a client running a months-old client would pin months-old
/// tools forever, with a config file that reads as a deliberate choice. Reading
/// the release list costs nothing this path does not already require — a
/// recipient who cannot reach the GitHub release host cannot install these tools
/// at all (see this module's "Known v1 risk").
///
/// # Postconditions
/// On `Ok`, every field of the returned [`ToolPins`] is an EXACT version that
/// was published as a stable release at the moment this ran. `latest` never
/// leaves this function: the caller writes these versions into
/// `engagement.toml`, and every run after the first reads the file. On `Err`,
/// no version is claimed for any tool and the caller must write nothing.
///
/// What: one [`trusty_installer::download::release::resolve_latest_tag`] call
/// per [`RequiredTool`], through the same HTTP client [`install`] uses. The
/// fields are named individually rather than mapped over [`RequiredTool::ALL`]
/// so a fifth tool fails to compile here as well as in [`RequiredTool::pin_in`].
/// Test: `super::tool_tests::an_unresolvable_pin_names_the_tool`.
///
/// # Errors
///
/// [`AuditError::PinsUnresolved`] naming the first tool whose release list could
/// not be read.
pub async fn latest_pins() -> Result<ToolPins, AuditError> {
    let client = trusty_installer::download::http_client();
    Ok(ToolPins {
        tga: latest_pin(&client, RequiredTool::Tga).await?,
        trusty_search: latest_pin(&client, RequiredTool::TrustySearch).await?,
        trusty_analyze: latest_pin(&client, RequiredTool::TrustyAnalyze).await?,
        trusty_review: latest_pin(&client, RequiredTool::TrustyReview).await?,
    })
}

/// One tool's latest published stable version, as a bare version pin.
///
/// No digest: nothing here has seen the artifact, and inventing a `sha256` the
/// caller did not obtain independently would turn the byte pin into decoration.
async fn latest_pin(client: &reqwest::Client, tool: RequiredTool) -> Result<ToolPin, AuditError> {
    trusty_installer::download::release::resolve_latest_tag(client, tool.crate_name())
        .await
        .map(|resolved| ToolPin::Version(resolved.version))
        .map_err(|source| AuditError::PinsUnresolved {
            tool: tool.crate_name(),
            source: source.into(),
        })
}

/// Whether each pinned tool is already here, and if not whether it could be.
///
/// Why: the cold-start launch (#5970) tells the recipient what it is about to
/// do before it spends minutes doing it, and a tool that cannot be installed at
/// all should surface in that first report rather than half way through a
/// download. The installability half is
/// [`trusty_installer::download::pinned::preflight_pinned_set`] — the capability
/// that issue added INSIDE the installer, so this crate still has exactly one
/// path that talks to a release list and exactly one that downloads.
///
/// # Postconditions
/// One [`ToolAvailability`] per [`RequiredTool::ALL`] entry, in that order.
/// Nothing was downloaded and nothing was written.
///
/// What: [`unsatisfied`] decides which tools need anything at all; only those
/// reach the network. A tool already on disk at its pin is reported
/// `installed: true` with no problem, because asking a release list about a
/// binary that is already verified answers a question nobody has.
/// Test: `super::tool_tests::preflight_asks_nothing_about_a_satisfied_set`.
///
/// # Errors
///
/// Whatever [`unsatisfied`] fails with — an unreadable or malformed version
/// record. A tool that cannot be installed is DATA here, not an error; turning
/// it into one is [`ensure_installable`]'s job.
pub async fn preflight(
    work: &WorkDir,
    pins: &ToolPins,
) -> Result<Vec<ToolAvailability>, AuditError> {
    let pending = unsatisfied(work, pins)?;
    let wanted: Vec<PinnedTool> = plan(pins)
        .into_iter()
        .zip(RequiredTool::ALL)
        .filter(|(_, tool)| pending.contains(tool))
        .map(|(pinned, _)| pinned)
        .collect();
    let mut checked = trusty_installer::download::pinned::preflight_pinned_set(
        &trusty_installer::download::http_client(),
        &wanted,
    )
    .await
    .into_iter();

    Ok(RequiredTool::ALL
        .iter()
        .map(|tool| {
            let satisfied = !pending.contains(tool);
            ToolAvailability {
                tool: *tool,
                version: tool.pin_in(pins).version().to_owned(),
                installed: satisfied,
                problem: if satisfied {
                    None
                } else {
                    checked.next().and_then(|c| c.problem).map(Box::new)
                },
            }
        })
        .collect())
}

/// Refuse a set that carries a tool this machine cannot install.
///
/// Why: the preflight is only worth running if something acts on it. Pure, so
/// the verdict is provable without a network.
/// What: the first problem, as [`AuditError::ToolNotInstallable`]. First rather
/// than all: the recipient's remedy — reach the release host, or ask for a
/// package built for their platform — is the same for every entry, and the
/// rendered report above it already listed them all.
/// Test: `super::tool_tests::a_tool_that_cannot_be_installed_is_refused_by_name`,
/// `super::tool_tests::an_installable_set_is_permitted`.
///
/// # Errors
///
/// [`AuditError::ToolNotInstallable`] naming the first refused tool.
pub fn ensure_installable(checked: Vec<ToolAvailability>) -> Result<(), AuditError> {
    for entry in checked {
        if let Some(problem) = entry.problem {
            return Err(AuditError::ToolNotInstallable {
                tool: entry.tool.crate_name(),
                source: problem,
            });
        }
    }
    Ok(())
}

/// Refuse an install target that is not a real directory.
///
/// Why: [`crate::workdir`] promises `rm -rf <root>` is a complete uninstall,
/// which holds only while every area is a directory inside the root. A symlink
/// planted at `tools/` before the first run sends the binaries somewhere else
/// and survives the delete.
/// What: `symlink_metadata`, which does NOT follow the link, so a symlink is
/// seen as a symlink rather than as its target.
/// Test: `super::tool_tests::a_symlinked_tools_area_is_refused`.
fn ensure_real_directory(path: &Path) -> Result<(), AuditError> {
    let meta = std::fs::symlink_metadata(path).map_err(|source| AuditError::WorkDir {
        path: path.to_path_buf(),
        source,
    })?;
    if meta.file_type().is_symlink() {
        return Err(AuditError::UnsafeToolsDir {
            path: path.to_path_buf(),
            kind: "symlink",
        });
    }
    if !meta.is_dir() {
        return Err(AuditError::UnsafeToolsDir {
            path: path.to_path_buf(),
            kind: "file",
        });
    }
    Ok(())
}

/// Where the version record lives.
pub fn record_path(work: &WorkDir) -> PathBuf {
    work.path(Area::State).join(VERSION_RECORD_FILE)
}

/// Read the recorded triple, or an empty list when nothing has installed yet.
///
/// # Errors
///
/// [`AuditError::Read`] when the file exists but cannot be read, and
/// [`AuditError::Parse`] when it is not a valid record.
pub fn read_record(work: &WorkDir) -> Result<Vec<InstalledTool>, AuditError> {
    let path = record_path(work);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(AuditError::Read { path, source }),
    };
    let record: VersionRecord = toml::from_str(&text).map_err(|source| AuditError::Parse {
        path,
        what: "tool version record",
        source: Box::new(source),
    })?;
    Ok(record.tools)
}

/// Record the triple that a successful install placed.
///
/// Why: closure condition 3 of #5495 — the deliverable states which versions
/// produced it. This is where that fact is captured; #5499 assembles the package
/// that carries it.
/// What: overwrites `state/`[`VERSION_RECORD_FILE`] with the whole set, since a
/// pinned install is all-or-nothing and a partial record would misdescribe it.
/// Test: `super::tool_tests::an_install_record_round_trips`.
///
/// # Errors
///
/// [`AuditError::WorkDir`] when the record cannot be written.
fn write_record(work: &WorkDir, installed: &[InstalledTool]) -> Result<(), AuditError> {
    let path = record_path(work);
    let record = VersionRecord {
        tools: installed.to_vec(),
    };
    // Infallible in practice: `VersionRecord` is a struct of owned strings and
    // paths with no map keys, so `toml` has no value it can refuse to render.
    let text = toml::to_string_pretty(&record).map_err(|e| AuditError::WorkDir {
        path: path.clone(),
        source: std::io::Error::other(e),
    })?;
    std::fs::write(&path, text).map_err(|source| AuditError::WorkDir { path, source })
}

#[cfg(test)]
mod tool_tests {
    use super::*;

    fn pins() -> ToolPins {
        crate::config::EngagementConfig::from_toml(
            r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks."

[tools]
tga = "2.9.4"
trusty-search = "0.47.0"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"
"#,
            Path::new("engagement.toml"),
        )
        .expect("the fixture config parses")
        .tools
    }

    fn install_of(crate_name: &str, version: &str) -> InstalledTool {
        InstalledTool {
            crate_name: crate_name.to_owned(),
            version: version.to_owned(),
            binary: PathBuf::from(format!("/work/tools/{crate_name}")),
        }
    }

    fn a_matching_set() -> Vec<InstalledTool> {
        vec![
            install_of("tga", "2.9.4"),
            install_of("trusty-search", "0.47.0"),
            install_of("trusty-analyze", "0.9.2"),
            install_of("trusty-review", "0.15.1"),
        ]
    }

    #[test]
    fn status_reports_a_tool_that_is_on_disk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = WorkDir::new(tmp.path().join("work"));
        work.create().expect("create");

        std::fs::write(RequiredTool::Tga.path_in(&work), b"#!/bin/sh\n").expect("write stub");

        let statuses = status(&work).expect("status reads");
        assert_eq!(statuses.len(), RequiredTool::ALL.len());
        let tga = statuses
            .iter()
            .find(|s| s.tool == RequiredTool::Tga)
            .expect("tga present in the report");
        assert!(tga.installed);
        // Nothing recorded it, so nothing may claim a version for it.
        assert_eq!(tga.version, None);
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
    fn the_plan_pins_every_tool_from_the_config() {
        let plan = plan(&pins());
        assert_eq!(plan.len(), RequiredTool::ALL.len());
        for (tool, pinned) in RequiredTool::ALL.iter().zip(&plan) {
            assert_eq!(pinned.crate_name, tool.crate_name());
            assert_eq!(pinned.binary, tool.binary_name());
        }
        let versions: Vec<&str> = plan.iter().map(|p| p.version.as_str()).collect();
        assert_eq!(versions, vec!["2.9.4", "0.47.0", "0.9.2", "0.15.1"]);
        // No digest was pinned, so none is invented.
        assert!(plan.iter().all(|p| p.sha256.is_none()));
    }

    #[test]
    fn a_digest_pin_reaches_the_installer() {
        let mut pins = pins();
        pins.tga = ToolPin::Digest {
            version: "2.9.4".to_owned(),
            sha256: "9f86d081884c7d65".to_owned(),
        };
        let plan = plan(&pins);
        assert_eq!(plan[0].sha256.as_deref(), Some("9f86d081884c7d65"));
    }

    #[test]
    fn a_matching_set_verifies() {
        let installed = verify(&pins(), &a_matching_set()).expect("a matching set verifies");
        assert_eq!(
            installed
                .iter()
                .map(|i| i.version.as_str())
                .collect::<Vec<_>>(),
            vec!["2.9.4", "0.47.0", "0.9.2", "0.15.1"]
        );
    }

    /// The #5454 defect class, refused from the install side.
    #[test]
    fn a_version_that_drifts_from_the_pin_is_refused() {
        let mut installs = a_matching_set();
        installs[3] = install_of("trusty-review", "0.14.0");
        let err = verify(&pins(), &installs).expect_err("a drifted version must not verify");
        let AuditError::VersionMismatch {
            tool,
            pinned,
            installed,
        } = err
        else {
            panic!("expected a version mismatch, got {err:?}");
        };
        assert_eq!(tool, "trusty-review");
        assert_eq!(pinned, "0.15.1");
        assert_eq!(installed, "0.14.0");
    }

    #[test]
    fn a_missing_install_is_refused() {
        let installs = vec![install_of("tga", "2.9.4")];
        let err = verify(&pins(), &installs).expect_err("a short set must not verify");
        assert!(matches!(
            err,
            AuditError::VersionMismatch {
                tool: "trusty-search",
                ..
            }
        ));
    }

    /// A reordered result is matched by name, never misattributed by position.
    #[test]
    fn results_are_matched_by_crate_name_not_position() {
        let mut installs = a_matching_set();
        installs.reverse();
        let installed = verify(&pins(), &installs).expect("order does not matter");
        assert_eq!(installed[0].crate_name, "tga");
        assert_eq!(installed[0].version, "2.9.4");
    }

    #[test]
    fn an_install_record_round_trips() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = WorkDir::new(tmp.path().join("work"));
        work.create().expect("create");

        let installed = verify(&pins(), &a_matching_set()).expect("verifies");
        write_record(&work, &installed).expect("record written");
        assert_eq!(read_record(&work).expect("record read"), installed);

        // And it shows up as the version `status` reports.
        std::fs::write(RequiredTool::Tga.path_in(&work), b"stub").expect("stub binary");
        let tga = status(&work).expect("status reads").remove(0);
        assert_eq!(tga.version.as_deref(), Some("2.9.4"));
    }

    #[test]
    fn an_absent_record_is_not_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = WorkDir::new(tmp.path().join("work"));
        work.create().expect("create");
        assert!(read_record(&work).expect("absent is fine").is_empty());
    }

    /// A record that cannot be trusted must not read as "installed, unknown".
    #[test]
    fn a_malformed_record_is_an_error_not_an_empty_list() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = WorkDir::new(tmp.path().join("work"));
        work.create().expect("create");
        std::fs::write(record_path(&work), "this is not toml = = =").expect("write");

        let err = read_record(&work).expect_err("a malformed record must fail");
        assert!(matches!(err, AuditError::Parse { .. }));
        assert!(
            status(&work).is_err(),
            "status must surface the same failure"
        );
    }

    #[test]
    fn a_real_tools_directory_passes_the_guard() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = WorkDir::new(tmp.path().join("work"));
        work.create().expect("create");
        ensure_real_directory(&work.path(Area::Tools)).expect("a real directory is fine");
    }

    #[test]
    fn a_file_where_the_tools_directory_belongs_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("work");
        std::fs::create_dir_all(&root).expect("mkdir");
        let work = WorkDir::new(&root);
        std::fs::write(work.path(Area::Tools), b"not a directory").expect("write");

        let err = ensure_real_directory(&work.path(Area::Tools)).expect_err("a file is refused");
        assert!(matches!(
            err,
            AuditError::UnsafeToolsDir { kind: "file", .. }
        ));
    }

    /// The escape `workdir.rs` deferred to this issue: a pre-planted symlink
    /// would put the recipient's tools outside the root that `rm -rf` cleans.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_tools_area_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("work");
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&root).expect("mkdir root");
        std::fs::create_dir_all(&elsewhere).expect("mkdir elsewhere");

        let work = WorkDir::new(&root);
        std::os::unix::fs::symlink(&elsewhere, work.path(Area::Tools)).expect("symlink");

        // `create` succeeds — `create_dir_all` follows the link to a real
        // directory — which is exactly why the guard cannot be folded into it.
        work.create()
            .expect("create follows the symlink without complaint");
        let err = ensure_real_directory(&work.path(Area::Tools)).expect_err("a symlink is refused");
        assert!(matches!(
            err,
            AuditError::UnsafeToolsDir {
                kind: "symlink",
                ..
            }
        ));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn install_refuses_a_symlinked_tools_area_before_any_download() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("work");
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&root).expect("mkdir root");
        std::fs::create_dir_all(&elsewhere).expect("mkdir elsewhere");
        let work = WorkDir::new(&root);
        std::os::unix::fs::symlink(&elsewhere, work.path(Area::Tools)).expect("symlink");

        let err = install(&work, &pins(), &Progress::none())
            .await
            .expect_err("the guard must fire before the network is touched");
        assert!(matches!(err, AuditError::UnsafeToolsDir { .. }));
        assert!(
            std::fs::read_dir(&elsewhere)
                .expect("read")
                .next()
                .is_none(),
            "nothing may be written through the symlink"
        );
        assert!(
            read_record(&work).expect("no record").is_empty(),
            "no record may claim an install that did not happen"
        );
    }

    /// Pins naming versions that were never published, so any download attempt
    /// fails at the release lookup — which is what makes "nothing was
    /// downloaded" assertable without a network.
    fn unpublishable_pins() -> ToolPins {
        crate::config::EngagementConfig::from_toml(
            r#"
openrouter_key = "sk-or-v1-not-a-real-key"
instructions = "Assess the last 52 weeks."

[tools]
tga = "0.0.0-never-published"
trusty-search = "0.0.0-never-published"
trusty-analyze = "0.0.0-never-published"
trusty-review = "0.0.0-never-published"
"#,
            Path::new("engagement.toml"),
        )
        .expect("the fixture config parses")
        .tools
    }

    /// Stub binaries plus a record claiming `version` for every tool — the
    /// state a successful install leaves behind.
    fn place_verified_set(work: &WorkDir, version: &str) {
        let mut record = String::new();
        for tool in RequiredTool::ALL {
            let path = tool.path_in(work);
            std::fs::write(&path, b"stub").expect("stub binary");
            record.push_str(&format!(
                "[[tools]]\ncrate_name = \"{}\"\nversion = \"{version}\"\nbinary = \"{}\"\n",
                tool.crate_name(),
                path.display()
            ));
        }
        std::fs::write(record_path(work), record).expect("write record");
    }

    fn work_in(dir: &Path) -> WorkDir {
        let work = WorkDir::new(dir.join("work"));
        work.create().expect("create");
        work
    }

    #[test]
    fn a_verified_set_at_the_pin_is_satisfied() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        place_verified_set(&work, "2.9.4");

        // Only tga is pinned at 2.9.4 in `pins()`, so pin the whole set there.
        let mut pins = pins();
        pins.trusty_search = ToolPin::Version("2.9.4".to_owned());
        pins.trusty_analyze = ToolPin::Version("2.9.4".to_owned());
        pins.trusty_review = ToolPin::Version("2.9.4".to_owned());

        assert!(
            unsatisfied(&work, &pins).expect("reads").is_empty(),
            "a set installed at the pin needs nothing"
        );
    }

    /// The `UNVERIFIED` state: a binary is there, nothing recorded it, so its
    /// version is unknown — and an unknown version is the #5454 skew input.
    #[test]
    fn an_unverified_binary_is_unsatisfied() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        for tool in RequiredTool::ALL {
            std::fs::write(tool.path_in(&work), b"stub").expect("stub binary");
        }

        let pending = unsatisfied(&work, &pins()).expect("reads");
        assert_eq!(
            pending,
            RequiredTool::ALL.to_vec(),
            "a present but unrecorded binary must be reinstalled, not kept"
        );
        // And `status` still calls it installed — the two answer different
        // questions, which is exactly why the file check alone is not enough.
        assert!(status(&work).expect("reads").iter().all(|s| s.installed));
    }

    /// The `MISSING` majority case, alongside one tool that is fine: the set is
    /// partly resolved, and a partial set is not a runnable one.
    #[test]
    fn a_partly_installed_set_is_unsatisfied_for_the_rest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        let path = RequiredTool::Tga.path_in(&work);
        std::fs::write(&path, b"stub").expect("stub binary");
        std::fs::write(
            record_path(&work),
            format!(
                "[[tools]]\ncrate_name = \"tga\"\nversion = \"2.9.4\"\nbinary = \"{}\"\n",
                path.display()
            ),
        )
        .expect("write record");

        let pending = unsatisfied(&work, &pins()).expect("reads");
        assert_eq!(
            pending,
            vec![
                RequiredTool::TrustySearch,
                RequiredTool::TrustyAnalyze,
                RequiredTool::TrustyReview
            ]
        );
    }

    /// Install and run are separate steps, so the config can move between them.
    #[test]
    fn a_recorded_version_behind_the_pin_is_unsatisfied() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        place_verified_set(&work, "0.0.1-stale");

        assert_eq!(
            unsatisfied(&work, &pins()).expect("reads"),
            RequiredTool::ALL.to_vec(),
            "a set recorded off the pin must be replaced"
        );
    }

    /// Idempotence: a satisfied set reaches no installer. The pins name a
    /// version that was never published, so a download would fail — `Ok(None)`
    /// is only reachable by not attempting one.
    #[tokio::test]
    async fn ensure_is_a_no_op_once_the_set_is_satisfied() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        place_verified_set(&work, "0.0.0-never-published");

        let placed = ensure(&work, &unpublishable_pins(), &Progress::none())
            .await
            .expect("a satisfied set is not reinstalled");
        assert!(placed.is_none(), "nothing may be downloaded: {placed:?}");
    }

    /// The other half of idempotence: an unsatisfied set DOES reach the
    /// installer. Proved without a network by the symlink guard, which
    /// `install` runs before it builds an HTTP client.
    #[tokio::test]
    #[cfg(unix)]
    async fn ensure_over_an_unsatisfied_set_reaches_the_installer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("work");
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&root).expect("mkdir root");
        std::fs::create_dir_all(&elsewhere).expect("mkdir elsewhere");
        let work = WorkDir::new(&root);
        std::os::unix::fs::symlink(&elsewhere, work.path(Area::Tools)).expect("symlink");

        let err = ensure(&work, &pins(), &Progress::none())
            .await
            .expect_err("an empty tools area must not be treated as satisfied");
        assert!(
            matches!(err, AuditError::UnsafeToolsDir { .. }),
            "ensure must reach install's guard, got {err:?}"
        );
    }

    /// The `UNVERIFIED` decision (#5797), pinned end to end: a binary that is
    /// present but that no version record names is REINSTALLED, not kept.
    /// `an_unverified_binary_is_unsatisfied` proves `unsatisfied` says so; this
    /// proves `ensure` acts on it, which is the half a later change could drop
    /// while leaving the predicate — and every other test here — green.
    ///
    /// Distinct from `ensure_over_an_unsatisfied_set_reaches_the_installer`,
    /// where `tools/` is EMPTY: that is the `MISSING` case, and a change that
    /// skipped present-but-unrecorded binaries would still pass it. Here the
    /// four binaries exist, so only the missing record makes the set unsatisfied.
    /// Same no-network proof: the symlink guard runs before `install` builds an
    /// HTTP client.
    #[tokio::test]
    #[cfg(unix)]
    async fn ensure_over_an_unverified_binary_reaches_the_installer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("work");
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&root).expect("mkdir root");
        std::fs::create_dir_all(&elsewhere).expect("mkdir elsewhere");
        let work = WorkDir::new(&root);
        std::os::unix::fs::symlink(&elsewhere, work.path(Area::Tools)).expect("symlink");

        // Every binary on disk, no version record: the UNVERIFIED state.
        for tool in RequiredTool::ALL {
            std::fs::write(tool.path_in(&work), b"stub").expect("stub binary");
        }
        assert!(
            status(&work).expect("reads").iter().all(|s| s.installed),
            "the set must be on disk, or this tests the MISSING case instead"
        );
        assert!(
            !record_path(&work).exists(),
            "nothing may have recorded them"
        );

        let err = ensure(&work, &pins(), &Progress::none())
            .await
            .expect_err("an unverifiable binary must be replaced, not kept");
        assert!(
            matches!(err, AuditError::UnsafeToolsDir { .. }),
            "ensure must reach install's guard, got {err:?}"
        );
    }

    /// A satisfied set reaches no release list (#5970). The pins name a version
    /// that was never published, so any lookup would fail — an empty problem
    /// list is only reachable by not asking.
    #[tokio::test]
    async fn preflight_asks_nothing_about_a_satisfied_set() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = work_in(tmp.path());
        place_verified_set(&work, "0.0.0-never-published");

        let checked = preflight(&work, &unpublishable_pins())
            .await
            .expect("a satisfied set preflights offline");

        assert_eq!(checked.len(), RequiredTool::ALL.len());
        assert!(
            checked.iter().all(|c| c.installed && c.problem.is_none()),
            "{checked:?}"
        );
        let versions: Vec<&str> = checked.iter().map(|c| c.version.as_str()).collect();
        assert_eq!(versions, vec!["0.0.0-never-published"; 4]);
    }

    /// The verdict a cold start acts on: one refused tool stops the set, by name.
    #[test]
    fn a_tool_that_cannot_be_installed_is_refused_by_name() {
        let checked = vec![
            ToolAvailability {
                tool: RequiredTool::Tga,
                version: "2.9.4".to_owned(),
                installed: true,
                problem: None,
            },
            ToolAvailability {
                tool: RequiredTool::TrustyReview,
                version: "0.0.1".to_owned(),
                installed: false,
                problem: Some(Box::new(
                    trusty_installer::download::pinned::PinnedError::VersionNotPublished {
                        crate_name: "trusty-review".to_owned(),
                        version: "0.0.1".to_owned(),
                        available: vec!["0.16.0".to_owned()],
                    },
                )),
            },
        ];

        let err = ensure_installable(checked).expect_err("a refused tool must stop the set");
        let AuditError::ToolNotInstallable { tool, .. } = &err else {
            panic!("expected ToolNotInstallable, got {err:?}");
        };
        assert_eq!(*tool, "trusty-review");
        let rendered = err.to_string();
        assert!(rendered.contains("trusty-review"), "{rendered}");
        assert!(rendered.contains("0.16.0"), "{rendered}");
    }

    #[test]
    fn an_installable_set_is_permitted() {
        let checked: Vec<ToolAvailability> = RequiredTool::ALL
            .iter()
            .map(|tool| ToolAvailability {
                tool: *tool,
                version: "1.2.3".to_owned(),
                installed: false,
                problem: None,
            })
            .collect();
        ensure_installable(checked).expect("nothing was refused");
    }

    /// A pin that cannot be resolved names the tool, and no version is invented
    /// for it — a guessed pin is the #5454 skew with a config file vouching for
    /// it.
    #[tokio::test]
    async fn an_unresolvable_pin_names_the_tool() {
        // A client with no route to the release host. `latest_pins` resolves in
        // `RequiredTool::ALL` order, so `tga` is what fails first.
        let err = latest_pin(
            &reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(1))
                .build()
                .expect("client"),
            RequiredTool::Tga,
        )
        .await
        .expect_err("a one-millisecond timeout cannot reach GitHub");

        let AuditError::PinsUnresolved { tool, .. } = &err else {
            panic!("expected PinsUnresolved, got {err:?}");
        };
        assert_eq!(*tool, "tga");
        assert!(
            err.to_string().contains("no engagement was created"),
            "{err}"
        );
    }

    #[test]
    fn install_errors_keep_the_installers_reason() {
        let source = trusty_installer::download::pinned::PinnedError::ChecksumMismatch {
            crate_name: "tga".to_owned(),
            version: "2.9.4".to_owned(),
            expected: "aaaa".to_owned(),
            actual: "bbbb".to_owned(),
        };
        let err = AuditError::Install {
            source: Box::new(source),
        };
        let rendered = err.to_string();
        assert!(rendered.contains("checksum mismatch"), "{rendered}");
        assert!(rendered.contains("nothing was installed"), "{rendered}");
    }
}
