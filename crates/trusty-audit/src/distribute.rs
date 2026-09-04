//! Assembling the install package that goes TO the recipient.
//!
//! Why: #5825. Everything else in this crate assumes the client is already on
//! the recipient's machine. Getting it there is a step nobody owned: the owner's
//! requirement is a zip they can drop in front of a client operator who extracts
//! it, runs one script, and needs no Rust toolchain, no `cargo install`, and no
//! PATH surgery to get from "downloaded a file" to "audit running".
//!
//! What: [`assemble`] writes one zip holding the `taudit` binary, a launcher
//! that runs it from wherever it was extracted, a generated `engagement.toml`,
//! a root README pointing at the instructions, and an `instructions/` directory
//! holding the sequence to run and a commented reference for every config key
//! (#5483). [`InstallPackage`] is what a front end renders.
//!
//! ## Direction — the reason this is not [`crate::package`]
//!
//! Two packages travel in opposite directions and their rules are opposites:
//!
//! | | [`crate::package`] (outbound) | this module (inbound) |
//! |---|---|---|
//! | Travels | recipient → auditor | auditor → recipient |
//! | Carries the OpenRouter key | **never** — refused per member | **deliberately**, in `engagement.toml` |
//! | Result type | [`crate::package::ReturnPackage`] | [`InstallPackage`] |
//!
//! The outbound refusal ([`AuditError::CredentialInPackage`]) is UNCHANGED and
//! unreferenced from here. This module does not call `package::assemble`, does
//! not take a direction flag, and shares no assembly code with it — the two are
//! separate functions producing separate types, because a boolean saying "this
//! one may carry the credential" is a boolean somebody eventually passes wrong.
//! The credential's one write path is [`crate::config::generate`], which lives
//! beside the [`SecretKey`](crate::config::SecretKey) whose missing `Serialize`
//! is what makes every OTHER write path a compile error.
//!
//! ## Fail-open is the failure mode this module is built against
//!
//! A half-assembled zip looks exactly like a deliverable. So every input is
//! checked before a byte is written, the archive is filled at a `.zip.part`
//! path that is removed on any failure, and the rename into place is the last
//! thing that happens. A refused assembly leaves the output directory as it
//! found it.
//!
//! Test: `super::distribute_tests`.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use crate::config::{self, EngagementConfig, SecretKey};
use crate::error::AuditError;
use crate::package::PackagedFile;
use crate::progress::{Operation, Progress, UnitOutcome};
use crate::registry::Target;
use crate::run::ENV_INFERENCE_CREDENTIAL;

pub mod instructions;

/// Where the package lands when the operator names no directory.
///
/// The owner's location, verbatim. The leading `~` is expanded by
/// [`expand_home`] rather than by a shell, so the default holds identically
/// when a GUI calls [`assemble`] with no shell anywhere in the picture.
pub const DEFAULT_OUTPUT_DIR: &str = "~/duetto/audit";

/// Filename of the install package.
pub const PACKAGE_FILE_NAME: &str = "trusty-audit-install.zip";

/// The single top-level directory inside the zip.
///
/// One root rather than loose members: extracting into a downloads folder that
/// already holds other files must not scatter four entries through it, and the
/// launcher resolves its siblings relative to itself, so the directory can be
/// renamed or moved anywhere.
pub const ROOT_PREFIX: &str = "trusty-audit";

/// The binary's name inside the package — the short form, which is what the
/// README tells the operator to type.
pub const BINARY_NAME: &str = "taudit";

/// The launcher the recipient runs.
pub const LAUNCHER_NAME: &str = "audit.sh";

/// The generated README member.
pub const README_NAME: &str = "README.md";

/// How much of the binary is read at a time while copying it.
const CHUNK_BYTES: usize = 64 * 1024;

/// How many members the package holds — the binary, the launcher, the config,
/// the pointer README, and the two under `instructions/`.
pub(crate) const MEMBER_COUNT: usize = 6;

/// Which report the packaged engagement is set up to produce (#5483).
///
/// Why: the CAST methodology is two lines in the engagement config (#6669), and
/// an auditor hand-adding them to their template before every handoff will
/// eventually not. A run that forgets them renders the default report and exits
/// 0, so the omission is invisible until the client reads the wrong document.
/// What: an enum rather than a `--cast` boolean, because the report template is
/// a NAME (`cast`, `default`, a bundled template) and the next preset is a
/// variant rather than a second flag that contradicts the first.
/// [`ReportPreset::Inherit`] is the default and writes nothing at all, so a
/// `distribute` run that names no preset behaves exactly as it did.
/// Test: `super::distribute_tests::the_cast_preset_writes_both_report_keys`,
/// `super::distribute_tests::the_default_preset_leaves_the_templates_report_table_alone`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ReportPreset {
    /// Whatever the template's own `[report]` table says. The default.
    #[default]
    Inherit,
    /// CAST-style technical due diligence, scoped to what a checkout can prove:
    /// `template = "cast"`, `code_only = true`.
    Cast,
}

impl ReportPreset {
    /// The `[report]` selection this preset writes, or `None` to write nothing.
    fn selection(self) -> Option<(&'static str, bool)> {
        match self {
            Self::Inherit => None,
            Self::Cast => Some(("cast", true)),
        }
    }
}

/// What the recipient chose about the package to build.
///
/// Why: both fields are `Option` because both have a defensible default the
/// operator should not have to restate — the owner's output directory, and the
/// binary currently running. Carrying them as data rather than reading argv or
/// the environment inside [`assemble`] is what keeps the Tauri shell able to
/// offer the same two choices through a form.
/// What: the output DIRECTORY (never the file — the filename is
/// [`PACKAGE_FILE_NAME`], so an operator cannot name two packages the same
/// thing by accident), and which binary to ship.
/// Test: `super::distribute_tests::the_default_output_directory_is_under_home`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct DistributeOptions {
    /// Directory to write the zip into. `None` means [`DEFAULT_OUTPUT_DIR`].
    pub output_dir: Option<PathBuf>,
    /// The `taudit` binary to ship. `None` means the running executable.
    pub binary: Option<PathBuf>,
    /// Ship NO credential, and let the recipient enter one (#5483).
    ///
    /// Why: the owner's handover conveys the key out of band, so the package
    /// that travels by email should not carry it. `true` writes a blank
    /// `openrouter_key`, which is exactly what
    /// [`crate::cli::credential::resolve`] turns into the first-run terminal
    /// prompt — there is no second mechanism, and no `key_env` indirection to
    /// keep in step with the schema (#5478 retired that spelling).
    /// What: it BEATS both the supplied key and the template's own, because it
    /// is the operator saying explicitly not to ship one. `false` is the
    /// existing behaviour, unchanged.
    pub prompt_for_key: bool,
    /// Which report the packaged engagement produces.
    pub report_preset: ReportPreset,
    /// A file naming the repositories the package declares (#5483).
    ///
    /// Why a PATH rather than a parsed list: `Cli::to_command` cannot fail, so
    /// the CLI carries the operator's choice as data and the fallible read
    /// happens here — the same shape `Command::AddTarget` uses for a target
    /// spec, and the reason `registry::parse` rather than clap decides what a
    /// target may be.
    /// What: `None` is the existing behaviour — the package declares no
    /// `targets` and the recipient registers each repository themselves. A path
    /// is read by [`crate::cli::targets_file::read_repo_list`], the crate's one
    /// repository-list parser, and written into the same `targets` key
    /// `taudit add repo` writes.
    pub repos: Option<PathBuf>,
}

/// The finished install package: what to send, and what it will run on.
///
/// Why: an operator about to hand a client a zip needs to know what is in it and
/// which machine it is for. `platform` is the field that makes the one failure
/// mode this package has visible before it ships rather than after: a binary
/// built for the wrong target extracts and runs on nothing.
/// What: the destination, every member, the sizes, and the host triple the
/// binary was built for.
/// Test: `super::distribute_tests::the_package_holds_every_member`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct InstallPackage {
    /// The finished zip. This is the file to send.
    pub path: PathBuf,
    /// Every member, in the order they were written.
    pub files: Vec<PackagedFile>,
    /// Total uncompressed size of the members.
    pub total_bytes: u64,
    /// Size of the zip on disk.
    pub packaged_bytes: u64,
    /// The OS/architecture the packaged binary runs on, e.g. `macos-aarch64`.
    pub platform: String,
    /// Whether the credential came from the environment rather than the
    /// template. Recorded so the operator can see which one they shipped.
    ///
    /// Always `false` when [`Self::prompts_for_key`] is set: no key shipped at
    /// all, so it did not come from the environment either.
    pub key_from_environment: bool,
    /// Whether the package ships no credential, leaving the recipient to enter
    /// one on first run (#5483).
    pub prompts_for_key: bool,
    /// Repositories the generated `engagement.toml` declares (#5483).
    ///
    /// Reported so the operator can confirm the list reached the package before
    /// they send it — the alternative is opening the zip.
    pub declared_repos: usize,
    /// Board-credential fields the template held that did NOT ship (#5861).
    ///
    /// Empty for a template that names no board. Reported rather than silent:
    /// an auditor whose template carries `[boards.jira]` would otherwise assume
    /// the recipient can collect that board on the first run.
    pub dropped_board_credentials: Vec<String>,
}

/// The host this build of the packaging binary targets.
///
/// Why: the default binary is the running executable, so its target IS this
/// one. When the operator names a binary explicitly they are asserting the
/// match, and the README says so rather than claiming a triple nothing checked.
fn host_platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Expand a leading `~` against the operator's home directory.
///
/// Why: [`DEFAULT_OUTPUT_DIR`] carries one, and a quoted `--out '~/elsewhere'`
/// reaches this crate unexpanded. Creating a literal `./~` directory is the
/// silent-wrong-place failure this exists to prevent, so an unresolvable home
/// is a refusal rather than a fallback.
/// What: replaces a leading `~` component only. `~user` is NOT expanded — it
/// needs a passwd lookup this crate has no business doing — and comes back as a
/// refusal for the same reason.
/// Test: `super::distribute_tests::expanding_a_tilde_reaches_the_home_directory`,
/// `super::distribute_tests::a_path_without_a_tilde_is_unchanged`.
///
/// # Errors
///
/// [`AuditError::MissingPackageInput`] when the path needs a home directory and
/// none resolves, or when it names another user's.
fn expand_home(path: &Path) -> Result<PathBuf, AuditError> {
    let mut parts = path.components();
    let Some(std::path::Component::Normal(first)) = parts.next() else {
        return Ok(path.to_path_buf());
    };
    if first != "~" {
        if first.to_string_lossy().starts_with('~') {
            return Err(AuditError::MissingPackageInput {
                what: "resolvable output directory (another user's ~ is not expanded)",
                path: path.to_path_buf(),
            });
        }
        return Ok(path.to_path_buf());
    }
    let home = dirs::home_dir().ok_or_else(|| AuditError::MissingPackageInput {
        what: "home directory to expand ~ against",
        path: path.to_path_buf(),
    })?;
    Ok(home.join(parts.as_path()))
}

/// Where the package lands, given what the operator chose.
///
/// Test: `super::distribute_tests::the_default_output_directory_is_under_home`,
/// `super::distribute_tests::a_chosen_directory_beats_the_default`.
///
/// # Errors
///
/// [`AuditError::MissingPackageInput`] when a `~` cannot be expanded.
pub fn destination(options: &DistributeOptions) -> Result<PathBuf, AuditError> {
    let chosen = options
        .output_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_OUTPUT_DIR));
    Ok(expand_home(&chosen)?.join(PACKAGE_FILE_NAME))
}

/// The credential to write into the generated config, and where it came from.
///
/// Why: `supplied` beats the template so the real key never has to sit on the
/// auditor's disk — the template can carry a blank placeholder and the key
/// arrives per invocation. Its source is the process environment, read by
/// [`crate::session::Session`] rather than here, which is what makes this
/// function pure and its tests free of a global every other test shares.
///
/// The environment rather than a CLI flag is deliberate: argv is visible to
/// every process on the machine through `ps`, and it lands in shell history.
/// Neither is true of a variable set for one command. There is no `--key` flag
/// and there should not be one.
/// What: `supplied` when it is not blank, else the template's own key when it is
/// not blank, else a refusal. The `bool` says which, so the operator can see
/// which key they shipped without the value being printed.
///
/// #5483: `prompt_for_key` is the third answer and it comes FIRST, ahead of
/// both. It is the operator saying not to ship a credential at all, which an
/// exported `OPENROUTER_API_KEY` left over from an earlier command must not
/// quietly override — that would bake a key into a package built precisely to
/// avoid carrying one, and nothing downstream would say so. The blank key it
/// returns is what [`crate::cli::credential::resolve`] reads as "ask the
/// recipient", so the two halves meet at the config field rather than at a
/// flag either side has to remember.
/// Test: `super::distribute_tests::a_supplied_key_beats_the_templates`,
/// `super::distribute_tests::a_blank_supplied_key_falls_back_to_the_template`,
/// `super::distribute_tests::a_blank_credential_is_refused_and_leaves_no_file`,
/// `super::distribute_tests::a_prompt_for_key_package_ships_no_credential`,
/// `super::distribute_tests::prompting_for_the_key_beats_a_supplied_one`.
fn resolve_key(
    supplied: Option<&SecretKey>,
    template: &EngagementConfig,
    template_path: &Path,
    prompt_for_key: bool,
) -> Result<(SecretKey, bool), AuditError> {
    if prompt_for_key {
        return Ok((SecretKey::new(""), false));
    }
    if let Some(key) = supplied
        && !key.is_empty()
    {
        return Ok((key.clone(), true));
    }
    if !template.openrouter_key.is_empty() {
        return Ok((template.openrouter_key.clone(), false));
    }
    Err(AuditError::MissingCredential {
        env: ENV_INFERENCE_CREDENTIAL,
        template: template_path.to_path_buf(),
    })
}

/// Build the install package.
///
/// Why: the one entry point, called only from
/// [`crate::session::Session::execute`] so the Tauri shell reaches it the same
/// way the CLI does (#5502).
/// What: validates every input, generates the config and the two text members,
/// then writes one zip. Every check happens before the temporary file is
/// created, so the common refusals never reach the filesystem at all; the
/// temporary covers the ones that can only fail mid-copy. `supplied_key` is the
/// credential the caller read from the environment — see [`resolve_key`] for
/// why it arrives as a parameter.
/// Test: `super::distribute_tests`.
///
/// # Errors
///
/// [`AuditError::MissingPackageInput`] for an absent binary or template,
/// [`AuditError::MissingCredential`] for no key, [`AuditError::PackageExists`]
/// when the destination is taken, [`AuditError::Distribute`] for a write that
/// failed, and [`AuditError::Parse`] for a template that is not a config.
pub fn assemble(
    template_path: &Path,
    options: &DistributeOptions,
    supplied_key: Option<&SecretKey>,
    progress: &Progress,
) -> Result<InstallPackage, AuditError> {
    let destination = destination(options)?;
    // #5825: refuse before anything else, so the operator learns the old
    // package is still there before waiting on a copy that will not land.
    if destination.exists() {
        return Err(AuditError::PackageExists { path: destination });
    }

    let binary = match &options.binary {
        Some(path) => path.clone(),
        None => std::env::current_exe().map_err(|source| AuditError::Distribute {
            path: PathBuf::from(BINARY_NAME),
            source,
        })?,
    };
    if !binary.is_file() {
        return Err(AuditError::MissingPackageInput {
            what: "taudit binary",
            path: binary,
        });
    }
    if !template_path.is_file() {
        return Err(AuditError::MissingPackageInput {
            what: "engagement config template",
            path: template_path.to_path_buf(),
        });
    }

    let template_text =
        std::fs::read_to_string(template_path).map_err(|source| AuditError::Read {
            path: template_path.to_path_buf(),
            source,
        })?;
    let template = EngagementConfig::from_toml(&template_text, template_path)?;
    let (key, key_from_environment) = resolve_key(
        supplied_key,
        &template,
        template_path,
        options.prompt_for_key,
    )?;
    // #5861: this config belongs to an engagement the template does not, so the
    // template's board credentials — the previous CLIENT's — never travel.
    let (mut config_text, dropped_board_credentials) =
        config::generate_for_new_engagement(&template_text, &key, template_path)?;
    // #5483: both are substitutions over the same table, applied in the order
    // an operator names them — what to audit, then which report to render.
    let repos: Vec<Target> = match &options.repos {
        Some(path) => crate::cli::targets_file::read_repo_list(path)?,
        None => Vec::new(),
    };
    if !repos.is_empty() {
        config_text = config::with_targets(&config_text, &repos, template_path)?;
    }
    if let Some((report_template, code_only)) = options.report_preset.selection() {
        config_text =
            config::with_report_selection(&config_text, report_template, code_only, template_path)?;
    }

    let platform = host_platform();
    let platform_line = platform_line(&platform, options.binary.is_some());
    let declared_repos = repos.len();
    let members = Members {
        config: config_text,
        launcher: render_launcher(),
        readme: instructions::render_pointer(&template, &platform_line),
        instructions: instructions::render_readme(
            &template,
            &platform_line,
            options.prompt_for_key,
            declared_repos,
        ),
        engagement_template: instructions::ENGAGEMENT_TEMPLATE.to_owned(),
    };

    progress.operation_started(Operation::Distribute, MEMBER_COUNT);
    let result = write_archive(&destination, &binary, members, progress);
    // #5825: the failing MEMBER is announced by the site that failed, inside
    // `fill_archive`, because only that site knows which one it was. This used
    // to report `BINARY_NAME` for every failure, so a README that could not be
    // written surfaced as "taudit failed". A failure with no member in flight —
    // creating the directory, finishing the archive, the rename — belongs to no
    // unit and names none.
    progress.operation_finished(
        Operation::Distribute,
        if result.is_ok() { MEMBER_COUNT } else { 0 },
        MEMBER_COUNT,
    );
    let (files, packaged_bytes) = result?;

    Ok(InstallPackage {
        path: destination,
        total_bytes: files.iter().map(|f| f.bytes).sum(),
        files,
        packaged_bytes,
        platform,
        key_from_environment,
        prompts_for_key: options.prompt_for_key,
        declared_repos,
        dropped_board_credentials,
    })
}

/// The line naming what the packaged binary runs on.
///
/// Why: the default binary is the running executable, so its target IS the
/// host. When the operator names one explicitly they are asserting the match,
/// and the line says so rather than claiming a triple nothing checked.
fn platform_line(platform: &str, explicit_binary: bool) -> String {
    if explicit_binary {
        format!("- Built for: {platform} (binary supplied by the auditor)")
    } else {
        format!("- Built for: {platform}")
    }
}

/// The generated text members, kept together so [`write_archive`]'s signature
/// does not grow five `String` parameters a caller can transpose.
struct Members {
    config: String,
    launcher: String,
    readme: String,
    instructions: String,
    engagement_template: String,
}

/// Fill a `.zip.part`, then rename it into place.
///
/// Why: the temporary is what makes "no partial zip" true rather than intended.
/// A copy that fails on its last chunk removes a `.part` file instead of leaving
/// a finished-looking package in the directory the operator attaches from.
/// Test: `super::distribute_tests::a_binary_that_cannot_be_read_leaves_no_partial_package`.
fn write_archive(
    destination: &Path,
    binary: &Path,
    members: Members,
    progress: &Progress,
) -> Result<(Vec<PackagedFile>, u64), AuditError> {
    let parent = destination.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|source| AuditError::Distribute {
        path: parent.to_path_buf(),
        source,
    })?;

    let temporary = destination.with_extension("zip.part");
    let files = match fill_archive(&temporary, binary, members, progress) {
        Ok(files) => files,
        Err(e) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(e);
        }
    };

    // Re-checked here rather than only up front: the whole point of the refusal
    // is that a package already sent survives, and `rename` would overwrite one
    // that appeared while this ran.
    if destination.exists() {
        let _ = std::fs::remove_file(&temporary);
        return Err(AuditError::PackageExists {
            path: destination.to_path_buf(),
        });
    }
    if let Err(source) = std::fs::rename(&temporary, destination) {
        let _ = std::fs::remove_file(&temporary);
        return Err(AuditError::Distribute {
            path: destination.to_path_buf(),
            source,
        });
    }

    let packaged_bytes = std::fs::metadata(destination).map(|m| m.len()).unwrap_or(0);
    Ok((files, packaged_bytes))
}

/// Announce that `unit` is the member that failed, then hand the error back.
///
/// Why: #5825. [`assemble`] cannot name the failing member — by the time an
/// error reaches it, which member was in flight is gone. So the site that fails
/// says so, mirroring the [`UnitOutcome::Succeeded`] call that sits beside it.
/// Test: `super::distribute_tests::a_failure_writing_a_member_names_that_member`,
/// `super::distribute_tests::a_binary_that_cannot_be_read_names_the_binary`.
fn failed_unit(progress: &Progress, unit: &str, error: AuditError) -> AuditError {
    progress.unit_finished(
        Operation::Distribute,
        unit,
        UnitOutcome::Failed(error.to_string()),
    );
    error
}

/// Write one zip entry's header.
fn start<W: std::io::Write + std::io::Seek>(
    zip: &mut zip::ZipWriter<W>,
    entry: &str,
    mode: u32,
    at: &Path,
) -> Result<(), AuditError> {
    let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(mode);
    zip.start_file(entry.to_owned(), options)
        .map_err(|e| AuditError::Distribute {
            path: at.to_path_buf(),
            source: std::io::Error::other(e),
        })
}

/// Every member, in the order the recipient meets them.
///
/// The binary is copied FIRST, because it is the member that can fail — the
/// three generated ones are already in memory. Failing early is what keeps the
/// window in which a `.part` exists as short as it can be.
fn fill_archive(
    temporary: &Path,
    binary: &Path,
    members: Members,
    progress: &Progress,
) -> Result<Vec<PackagedFile>, AuditError> {
    let file = std::fs::File::create(temporary).map_err(|source| AuditError::Distribute {
        path: temporary.to_path_buf(),
        source,
    })?;
    write_members(
        std::io::BufWriter::new(file),
        temporary,
        binary,
        members,
        progress,
    )
}

/// [`fill_archive`], with the archive's destination as an argument.
///
/// Why: #5825 — every per-member failure this writes a `Failed` for is an I/O
/// error, and ENOSPC on the third member is not something a test can arrange
/// through the filesystem. Taking the sink makes the mid-archive arm provable
/// with a writer that fails on cue, the same shape as
/// [`crate::run`]'s `sweep_with_env` taking the environment rather than
/// reading it.
/// Test: `super::distribute_tests::a_failure_writing_a_member_names_that_member`.
fn write_members<W>(
    sink: W,
    temporary: &Path,
    binary: &Path,
    members: Members,
    progress: &Progress,
) -> Result<Vec<PackagedFile>, AuditError>
where
    W: std::io::Write + std::io::Seek,
{
    let mut zip = zip::ZipWriter::new(sink);
    let mut files = Vec::with_capacity(MEMBER_COUNT);

    progress.unit_started(Operation::Distribute, BINARY_NAME, 1, MEMBER_COUNT);
    let entry = entry_path(BINARY_NAME);
    let bytes = copy_binary(&mut zip, &entry, binary, temporary)
        .map_err(|e| failed_unit(progress, BINARY_NAME, e))?;
    files.push(PackagedFile {
        entry,
        source: Some(binary.to_path_buf()),
        bytes,
    });
    progress.unit_finished(Operation::Distribute, BINARY_NAME, UnitOutcome::Succeeded);

    // 0o600 on the config: it carries the credential, and an extracted
    // world-readable copy on a shared machine is a leak the zip can prevent.
    for (index, (name, text, mode)) in [
        (LAUNCHER_NAME, members.launcher, 0o755),
        (EngagementConfig::FILE_NAME, members.config, 0o600),
        (README_NAME, members.readme, 0o644),
        (
            instructions::INSTRUCTIONS_README,
            members.instructions,
            0o644,
        ),
        (
            instructions::ENGAGEMENT_TEMPLATE_NAME,
            members.engagement_template,
            0o644,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        progress.unit_started(Operation::Distribute, name, index + 2, MEMBER_COUNT);
        let entry = entry_path(name);
        start(&mut zip, &entry, mode, temporary).map_err(|e| failed_unit(progress, name, e))?;
        zip.write_all(text.as_bytes()).map_err(|source| {
            failed_unit(
                progress,
                name,
                AuditError::Distribute {
                    path: temporary.to_path_buf(),
                    source,
                },
            )
        })?;
        files.push(PackagedFile {
            entry,
            source: None,
            bytes: text.len() as u64,
        });
        progress.unit_finished(Operation::Distribute, name, UnitOutcome::Succeeded);
    }

    zip.finish().map_err(|e| AuditError::Distribute {
        path: temporary.to_path_buf(),
        source: std::io::Error::other(e),
    })?;
    Ok(files)
}

/// A member's path inside the zip, under the single root.
fn entry_path(name: &str) -> String {
    format!("{ROOT_PREFIX}/{name}")
}

/// Copy the binary into the archive.
///
/// Streamed rather than read whole: a debug-profile `taudit` runs to hundreds of
/// megabytes, and holding it in memory to write it out again buys nothing.
fn copy_binary<W: std::io::Write + std::io::Seek>(
    zip: &mut zip::ZipWriter<W>,
    entry: &str,
    source: &Path,
    temporary: &Path,
) -> Result<u64, AuditError> {
    let mut input = std::fs::File::open(source).map_err(|e| AuditError::Distribute {
        path: source.to_path_buf(),
        source: e,
    })?;
    start(zip, entry, 0o755, temporary)?;
    let mut buffer = vec![0_u8; CHUNK_BYTES];
    let mut written = 0_u64;
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|e| AuditError::Distribute {
                path: source.to_path_buf(),
                source: e,
            })?;
        if read == 0 {
            break;
        }
        zip.write_all(&buffer[..read])
            .map_err(|source| AuditError::Distribute {
                path: temporary.to_path_buf(),
                source,
            })?;
        written += read as u64;
    }
    Ok(written)
}

/// The launcher the recipient runs from the extracted directory.
///
/// Why: requirement 3 of #5825 — the package must run with no prior install
/// step. Every path is resolved relative to the SCRIPT, not the working
/// directory, so the extracted folder can be moved or renamed and double-clicked
/// from anywhere. It never consults `PATH`: the binary the recipient runs is the
/// one in the package, and a `taudit` already installed on their machine at some
/// other version must not win.
/// What: POSIX `sh`, `set -eu`, `exec` so the exit status the client returns is
/// the one the shell sees.
///
/// #5915: it no longer pins `--work-dir "$here/work"`. That pin put the run's
/// tree wherever the recipient unzipped the package, and it therefore DECIDED
/// the placement for every packaged run — the `WorkDir::resolve` default was
/// unreachable from the only flow that ships. `trusty-search` refuses to index a
/// checkout under `/tmp`, `/var/folders`, `~/Downloads`, `~/Desktop` or
/// `~/Documents`, which is where an emailed zip gets opened, so the pin cost the
/// code-analysis leg its data. Dropping it lets the default
/// (`~/.trusty-tools/trusty-audit/work`) apply; `--work-dir` and
/// `TRUSTY_AUDIT_WORKDIR` still override, and the recipient can still pass
/// `--work-dir` through this launcher because `"$@"` is forwarded.
///
/// What this trades: the tree is no longer beside the launcher, so deleting the
/// extracted folder no longer removes it. [`render_readme`] names the new
/// location and the command that removes it.
/// Test: `super::distribute_tests::the_launcher_resolves_everything_beside_itself`,
/// `super::distribute_tests::the_launcher_leaves_the_work_root_to_the_client`.
fn render_launcher() -> String {
    format!(
        "#!/bin/sh\n\
         # Runs the audit client from this directory. No install step, no PATH.\n\
         set -eu\n\
         here=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\n\
         exec \"$here/{BINARY_NAME}\" \\\n\
         \x20 --config \"$here/{config}\" \\\n\
         \x20 \"$@\"\n",
        config = EngagementConfig::FILE_NAME,
    )
}

#[cfg(test)]
mod distribute_tests {
    use super::*;
    use crate::progress::{ProgressUpdate, Recorder};

    const TEMPLATE: &str = r#"
openrouter_key = "sk-or-v1-template-key"
instructions = "Assess the last 52 weeks."
client = "Acme"
engagement = "2026-Q3"

[tools]
tga = "2.9.4"
trusty-search = "0.4.0"
trusty-analyze = "0.9.2"
trusty-review = "0.16.0"
"#;

    /// A template with a blank key — the shape an auditor keeps on disk so the
    /// real credential only ever arrives through the environment.
    const BLANK_TEMPLATE: &str = r#"
openrouter_key = ""
instructions = "Assess the last 52 weeks."

[tools]
tga = "2.9.4"
trusty-search = "0.4.0"
trusty-analyze = "0.9.2"
trusty-review = "0.16.0"
"#;

    /// A fixture engagement: a template file, a stand-in binary, and an empty
    /// output directory.
    struct Fixture {
        _dir: tempfile::TempDir,
        template: PathBuf,
        binary: PathBuf,
        out: PathBuf,
    }

    impl Fixture {
        fn new(template_text: &str) -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let template = dir.path().join("template.toml");
            std::fs::write(&template, template_text).expect("write template");
            let binary = dir.path().join("taudit-fixture");
            std::fs::write(&binary, b"\x7fELF not really a binary, but bytes to copy")
                .expect("write binary");
            let out = dir.path().join("out");
            Self {
                _dir: dir,
                template,
                binary,
                out,
            }
        }

        fn options(&self) -> DistributeOptions {
            DistributeOptions {
                output_dir: Some(self.out.clone()),
                binary: Some(self.binary.clone()),
                ..DistributeOptions::default()
            }
        }

        fn assemble(&self) -> Result<InstallPackage, AuditError> {
            self.assemble_with(None)
        }

        fn assemble_with(&self, key: Option<&SecretKey>) -> Result<InstallPackage, AuditError> {
            super::assemble(&self.template, &self.options(), key, &Progress::none())
        }

        /// Every filename in the output directory, sorted. Empty when the
        /// directory does not exist — which is itself the clean state.
        fn out_entries(&self) -> Vec<String> {
            let Ok(read) = std::fs::read_dir(&self.out) else {
                return Vec::new();
            };
            let mut names: Vec<String> = read
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    /// Read one member out of a written package.
    fn member(zip_path: &Path, entry: &str) -> String {
        let file = std::fs::File::open(zip_path).expect("open package");
        let mut archive = zip::ZipArchive::new(file).expect("read package");
        let mut member = archive.by_name(entry).expect("member present");
        let mut text = String::new();
        member.read_to_string(&mut text).expect("member is text");
        text
    }

    /// Every unit the recorder saw finish, as `(member, "ok" | "failed")`.
    fn unit_verdicts(recorder: &Recorder) -> Vec<(String, &'static str)> {
        recorder
            .updates()
            .into_iter()
            .filter_map(|u| match u {
                ProgressUpdate::UnitFinished {
                    target, outcome, ..
                } => Some((target, outcome.label())),
                _ => None,
            })
            .collect()
    }

    /// How the operation as a whole was reported to have ended.
    fn operation_verdict(recorder: &Recorder) -> Option<(usize, usize)> {
        recorder.updates().into_iter().find_map(|u| match u {
            ProgressUpdate::OperationFinished {
                succeeded, total, ..
            } => Some((succeeded, total)),
            _ => None,
        })
    }

    /// A writer that refuses the moment `marker` appears in the bytes it is
    /// handed.
    ///
    /// Why: every per-member failure inside [`write_members`] is an I/O error,
    /// and ENOSPC on the third member is not something a filesystem fixture can
    /// arrange. A zip's local file header carries the member's name in
    /// plaintext, so refusing on that name puts the failure precisely inside
    /// that member and nowhere else.
    struct FailAtMember {
        written: std::io::Cursor<Vec<u8>>,
        marker: Vec<u8>,
    }

    impl std::io::Write for FailAtMember {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if buf.windows(self.marker.len()).any(|w| w == self.marker) {
                return Err(std::io::Error::other("no space left on device"));
            }
            self.written.write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.written.flush()
        }
    }

    impl std::io::Seek for FailAtMember {
        fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
            self.written.seek(to)
        }
    }

    /// A terminal that answers every hidden read with the same key.
    ///
    /// Local rather than reusing `credential_tests`' fake: that one lives in
    /// another module's `#[cfg(test)]` body and is not reachable from here.
    /// This drives the public [`crate::cli::credential::Tty`] trait, so it
    /// proves the same seam.
    struct FakeTty(String);

    impl FakeTty {
        fn answering(key: &str) -> Self {
            Self(key.to_owned())
        }
    }

    impl crate::cli::credential::Tty for FakeTty {
        fn read_hidden(&mut self, _prompt: &str) -> std::io::Result<String> {
            Ok(self.0.clone())
        }

        fn read_line(&mut self, _prompt: &str) -> std::io::Result<Option<String>> {
            Ok(None)
        }

        fn say(&mut self, _line: &str) -> std::io::Result<()> {
            Ok(())
        }

        fn discard_typeahead(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// One member's extracted permission bits, or `None` off Unix.
    fn mode_of(zip_path: &Path, name: &str) -> Option<u32> {
        let file = std::fs::File::open(zip_path).expect("open package");
        let mut archive = zip::ZipArchive::new(file).expect("read package");
        let member = archive.by_name(&entry_path(name)).expect("member present");
        // `unix_mode` carries the file-type bits too (0o100755, not 0o755).
        member.unix_mode().map(|m| m & 0o777)
    }

    /// Every entry name in a written package, in order.
    fn entries(zip_path: &Path) -> Vec<String> {
        let file = std::fs::File::open(zip_path).expect("open package");
        let mut archive = zip::ZipArchive::new(file).expect("read package");
        (0..archive.len())
            .map(|i| archive.by_index(i).expect("entry").name().to_owned())
            .collect()
    }

    #[test]
    fn the_package_holds_every_member() {
        let fixture = Fixture::new(TEMPLATE);
        let package = fixture.assemble().expect("assembles");

        assert_eq!(
            entries(&package.path),
            vec![
                "trusty-audit/taudit",
                "trusty-audit/audit.sh",
                "trusty-audit/engagement.toml",
                "trusty-audit/README.md",
                "trusty-audit/instructions/README.md",
                "trusty-audit/instructions/engagement.template.toml",
            ],
        );
        assert_eq!(package.files.len(), MEMBER_COUNT);
        assert!(package.packaged_bytes > 0);
        assert_eq!(package.platform, host_platform());
    }

    #[test]
    fn the_generated_config_carries_the_credential_and_still_loads() {
        let fixture = Fixture::new(TEMPLATE);
        let package = fixture.assemble().expect("assembles");

        let text = member(&package.path, "trusty-audit/engagement.toml");
        assert!(
            text.contains("sk-or-v1-template-key"),
            "the inbound package deliberately carries the key: {text}"
        );
        let loaded = EngagementConfig::from_toml(&text, Path::new("engagement.toml"))
            .expect("the generated config loads on the recipient's machine");
        assert_eq!(loaded.openrouter_key.expose(), "sk-or-v1-template-key");
        assert_eq!(loaded.tools.tga.version(), "2.9.4");
    }

    /// An auditor's template as it looks after a previous engagement: the last
    /// client's board credentials are still in it.
    const TEMPLATE_WITH_STALE_BOARDS: &str = r#"
openrouter_key = "sk-or-v1-template-key"
instructions = "Assess the last 52 weeks."
client = "Acme"

[tools]
tga = "2.9.4"
trusty-search = "0.4.0"
trusty-analyze = "0.9.2"
trusty-review = "0.16.0"

[boards.jira]
url = "https://client-a.atlassian.net"
email = "auditor@client-a.example"
token = "jira-token-client-a"

[boards.linear]
api_key = "lin_api_client_a"
"#;

    /// 🔴 #5861: the zip that goes to client B must not carry client A's board
    /// credentials.
    ///
    /// This is the leak end to end, through the file that actually ships.
    /// Against `6d937ba66` `config::generate` copied the template wholesale, so
    /// `engagement.toml` inside the package held `jira-token-client-a` — and
    /// that file is deliberately readable, which is the whole transparency
    /// premise of the handoff. `crate::package`'s outbound credential scan
    /// cannot catch it: that guards the package coming BACK.
    #[test]
    fn a_reused_template_never_ships_the_previous_clients_board_credentials() {
        let fixture = Fixture::new(TEMPLATE_WITH_STALE_BOARDS);
        let package = fixture.assemble().expect("assembles");

        let text = member(&package.path, "trusty-audit/engagement.toml");
        for secret in [
            "jira-token-client-a",
            "lin_api_client_a",
            "client-a.atlassian.net",
            "auditor@client-a.example",
        ] {
            assert!(
                !text.contains(secret),
                "the package carries the previous client's {secret}: {text}"
            );
        }
        assert_eq!(
            package.dropped_board_credentials,
            ["boards.jira", "boards.linear"]
        );

        // Nothing else about the engagement changed, and the config still runs.
        let loaded =
            EngagementConfig::from_toml(&text, Path::new("engagement.toml")).expect("loads");
        assert_eq!(loaded.openrouter_key.expose(), "sk-or-v1-template-key");
        assert_eq!(loaded.tools.tga.version(), "2.9.4");
        assert!(loaded.boards.jira.is_none());
        assert!(loaded.boards.linear.is_none());
    }

    /// A template that names no board reports nothing dropped, so the operator
    /// only sees the line when it is about their template.
    #[test]
    fn a_template_with_no_boards_reports_nothing_dropped() {
        let fixture = Fixture::new(TEMPLATE);
        let package = fixture.assemble().expect("assembles");
        assert!(
            package.dropped_board_credentials.is_empty(),
            "{:?}",
            package.dropped_board_credentials
        );
    }

    #[test]
    fn the_launcher_resolves_everything_beside_itself() {
        let fixture = Fixture::new(TEMPLATE);
        let package = fixture.assemble().expect("assembles");

        let script = member(&package.path, "trusty-audit/audit.sh");
        assert!(script.starts_with("#!/bin/sh\n"), "{script}");
        assert!(script.contains("here=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)"));
        assert!(script.contains("exec \"$here/taudit\""), "{script}");
        assert!(
            script.contains("--config \"$here/engagement.toml\""),
            "{script}"
        );
        // The binary is never named bare: every occurrence is `$here/`-prefixed,
        // so a `taudit` already on PATH at some other version cannot win.
        for (index, _) in script.match_indices(BINARY_NAME) {
            assert!(
                script[..index].ends_with("$here/"),
                "the launcher must never reach for an installed taudit: {script}"
            );
        }
    }

    /// #5915: the launcher used to pass `--work-dir "$here/work"`, which decided
    /// the placement for every packaged run and made `WorkDir::resolve`'s default
    /// dead code. A checkout under the unzip location is one `trusty-search`
    /// refuses to index, so the code-analysis leg came back empty on every real
    /// recipient machine. The launcher must now name no work directory at all
    /// and forward whatever the recipient passes.
    #[test]
    fn the_launcher_leaves_the_work_root_to_the_client() {
        let fixture = Fixture::new(TEMPLATE);
        let package = fixture.assemble().expect("assembles");

        let script = member(&package.path, "trusty-audit/audit.sh");
        assert!(
            !script.contains("--work-dir"),
            "the launcher pins the work root again: {script}"
        );
        assert!(
            script.contains("\"$@\""),
            "the recipient can no longer pass --work-dir through: {script}"
        );
    }

    /// Read the instructions member out of a written package.
    fn instructions_of(package: &InstallPackage) -> String {
        member(
            &package.path,
            &entry_path(instructions::INSTRUCTIONS_README),
        )
    }

    /// The uninstall step is the only place the recipient learns that the tree
    /// is not beside the launcher and that approving a clone wrote a row
    /// outside it (#5915). It moved into `instructions/` with the rest (#5483).
    #[test]
    fn the_instructions_name_where_the_work_root_is_and_how_to_undo_the_approval() {
        let fixture = Fixture::new(TEMPLATE);
        let package = fixture.assemble().expect("assembles");

        let readme = instructions_of(&package);
        assert!(
            readme.contains(crate::workdir::DEFAULT_ROOT_DISPLAY),
            "{readme}"
        );
        assert!(readme.contains("trusty-search index remove"), "{readme}");
        assert!(
            !readme.contains(
                "deleting the folder\nremoves the client, its tooling, and everything it wrote"
            ),
            "the instructions still promise a complete uninstall they cannot deliver: {readme}"
        );
    }

    /// #5483: the recipient is walked through install, then the ONE-SHOT
    /// `audit` verb, then `package` — not the `run` + `package` pair the old
    /// root README named, which predated `audit` (#5824) and left a
    /// non-technical recipient a step they could forget or run too early.
    ///
    /// The Full Disk Access paragraph is here for the same reason the
    /// quarantine one is: without it the code-analysis sections render empty
    /// and nothing on the recipient's screen says why (root `CLAUDE.md`,
    /// "macOS TCC scope split").
    #[test]
    fn the_instructions_name_the_one_shot_verb_and_the_disk_access_prompt() {
        let fixture = Fixture::new(TEMPLATE);
        let package = fixture.assemble().expect("assembles");

        let readme = instructions_of(&package);
        let install = readme.find("./audit.sh install").expect("installs tools");
        let audit = readme.find("./audit.sh audit").expect("runs the audit");
        let ret = readme
            .find("./audit.sh package")
            .expect("returns the package");
        assert!(
            install < audit && audit < ret,
            "the commands are out of order: {readme}"
        );
        assert!(
            !readme.contains("./audit.sh run"),
            "the instructions still route through the two-step run: {readme}"
        );
        assert!(readme.contains("Acme — 2026-Q3"), "{readme}");
        assert!(readme.contains("com.apple.quarantine"), "{readme}");
        assert!(readme.contains("Full Disk Access"), "{readme}");
        assert!(readme.contains("gh auth login"), "{readme}");
        // "If something fails" must name where the logs are, or a stopped run
        // leaves the recipient with nothing to send back.
        assert!(readme.contains("/logs/"), "{readme}");
        assert!(readme.contains(&host_platform()), "{readme}");
    }

    /// #5473, #5483: the recipient must fill in the placeholder engagement
    /// fields and register a repository BEFORE installing or auditing
    /// anything, not after — a recipient who runs `install` first still has
    /// no idea the config needs editing at all.
    #[test]
    fn the_engagement_setup_step_precedes_install_and_names_the_fields_to_replace() {
        let fixture = Fixture::new(TEMPLATE);
        let package = fixture.assemble().expect("assembles");

        let readme = instructions_of(&package);
        let setup = readme
            .find("Fill in the engagement")
            .expect("names the setup step");
        let install = readme.find("./audit.sh install").expect("installs tools");
        assert!(
            setup < install,
            "engagement setup must come before install: {readme}"
        );

        // (i) which fields to replace, with a worked example.
        assert!(
            readme.contains("Set `client`, `engagement`, and `instructions`"),
            "{readme}"
        );
        assert!(readme.contains("client = \"Example Corp\""), "{readme}");

        // (ii) the exact `[[targets]]` shape, for the paste-in alternative to
        // `add repo`.
        assert!(readme.contains("[[targets]]"), "{readme}");
        assert!(readme.contains("kind = \"repo\""), "{readme}");
        assert!(
            readme.contains("name_with_owner = \"acme/api\""),
            "{readme}"
        );
        assert!(readme.contains("./audit.sh remove"), "{readme}");

        // (iii) private-repo access rides the recipient's own `gh` credential.
        assert!(readme.contains("gh auth login"), "{readme}");

        // (iv) what a zero-target run actually does.
        assert!(readme.contains("nothing to audit"), "{readme}");
        assert!(
            readme.contains("resumes the existing selection"),
            "the no-target note must name the fallback-to-prior-selection case: {readme}"
        );
    }

    /// #5483: one source of truth. The root README carries the platform and the
    /// pointer, and must not grow a second copy of the sequence.
    #[test]
    fn the_root_readme_points_at_the_instructions() {
        let fixture = Fixture::new(TEMPLATE);
        let package = fixture.assemble().expect("assembles");

        let readme = member(&package.path, "trusty-audit/README.md");
        assert!(
            readme.contains(instructions::INSTRUCTIONS_README),
            "{readme}"
        );
        assert!(readme.contains("Acme — 2026-Q3"), "{readme}");
        assert!(readme.contains(&host_platform()), "{readme}");
        assert!(
            !readme.contains("./audit.sh add repo"),
            "the root README carries the sequence again: {readme}"
        );
    }

    /// The shipped reference must be a config the client can actually load —
    /// an auditor copies it, fills in the pins, and hands it back to
    /// `taudit distribute --config`.
    #[test]
    fn the_shipped_config_reference_is_itself_loadable() {
        let fixture = Fixture::new(TEMPLATE);
        let package = fixture.assemble().expect("assembles");

        let text = member(
            &package.path,
            &entry_path(instructions::ENGAGEMENT_TEMPLATE_NAME),
        );
        let loaded = EngagementConfig::from_toml(&text, Path::new("engagement.template.toml"))
            .expect("the shipped reference loads");
        assert!(
            loaded.openrouter_key.is_empty(),
            "the reference must ship no credential"
        );
        assert_eq!(loaded.report.template.as_deref(), Some("cast"));
        assert_eq!(loaded.report.code_only, Some(true));
        assert_eq!(loaded.models.provider.as_deref(), Some("openrouter"));
        assert!(loaded.models.reviewer.is_some());
        assert!(loaded.models.verifier.is_some());
        assert!(loaded.models.summarizer.is_some());
        assert!(
            loaded.unsupported_keys().is_empty(),
            "the reference declares a key this version ignores: {:?}",
            loaded.unsupported_keys()
        );
        // The commented `[[targets]]` example the operator uncomments.
        assert!(text.contains("name_with_owner = \"acme/api\""), "{text}");
        assert!(text.contains("name_with_owner = \"acme/web\""), "{text}");
    }

    /// The constant `taudit distribute` ships is the file on disk, not a copy.
    ///
    /// Why: #6772 — `scripts/refresh-engagement-pins.sh` and its
    /// `preflight-publish.sh` CHECK 10 both edit and read the FILE, while every
    /// packaged copy comes from the CONSTANT. `include_str!` already makes
    /// those the same bytes at compile time, so this assertion is close to
    /// tautological; what it actually guards is the provenance, catching a
    /// future edit that replaces the `include_str!` with an inline literal and
    /// puts the release gate back to checking a file nothing ships.
    #[test]
    fn the_compiled_engagement_template_is_the_file_on_disk() {
        // #6772: the release gate edits this path; the binary ships the constant.
        let on_disk = std::fs::read_to_string(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/templates/engagement.template.toml"
        )))
        .expect("the template is a tracked file in this crate");

        assert_eq!(
            instructions::ENGAGEMENT_TEMPLATE,
            on_disk,
            "ENGAGEMENT_TEMPLATE no longer comes from \
             templates/engagement.template.toml, so refreshing that file would \
             leave the packaged copy stale (#6772)"
        );
    }

    /// Every `[tools]` pin the shipped reference carries is a version triple.
    ///
    /// Why: #6772 left the template pinning tga 6.0.0 while the same release
    /// train published 7.0.0. `scripts/refresh-engagement-pins.sh` rewrites
    /// these literals with an awk substitution, so a mangled rewrite would
    /// otherwise ship a pin like `7.1.0"` that no download can resolve. This
    /// asserts the shape the client's own installer needs, on the constant the
    /// client actually gets.
    #[test]
    fn every_tool_pin_in_the_shipped_template_is_a_semver_triple() {
        // #6772: refresh-engagement-pins.sh rewrites these literals in place.
        let loaded = EngagementConfig::from_toml(
            instructions::ENGAGEMENT_TEMPLATE,
            Path::new("engagement.template.toml"),
        )
        .expect("the shipped reference loads");

        let pins = [
            ("tga", loaded.tools.tga.version()),
            ("trusty-search", loaded.tools.trusty_search.version()),
            ("trusty-analyze", loaded.tools.trusty_analyze.version()),
            ("trusty-review", loaded.tools.trusty_review.version()),
        ];

        for (name, version) in pins {
            assert!(
                is_version_triple(version),
                "the {name} pin is not a version triple: {version:?}"
            );
        }
    }

    /// `MAJOR.MINOR.PATCH`, with SemVer's optional `-pre` and `+build` tails.
    fn is_version_triple(version: &str) -> bool {
        let core = version.split('+').next().unwrap_or_default();
        let core = core.split('-').next().unwrap_or_default();
        let parts: Vec<&str> = core.split('.').collect();
        parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    }

    #[test]
    fn a_missing_binary_is_refused_and_leaves_no_file() {
        let fixture = Fixture::new(TEMPLATE);
        let mut options = fixture.options();
        options.binary = Some(fixture.binary.with_file_name("absent"));

        let error = super::assemble(&fixture.template, &options, None, &Progress::none())
            .expect_err("a package with no binary is not a package");
        assert!(
            matches!(
                error,
                AuditError::MissingPackageInput {
                    what: "taudit binary",
                    ..
                }
            ),
            "{error}"
        );
        assert!(fixture.out_entries().is_empty(), "left a partial package");
    }

    #[test]
    fn a_missing_template_is_refused_and_leaves_no_file() {
        let fixture = Fixture::new(TEMPLATE);
        let absent = fixture.template.with_file_name("absent.toml");

        let error = super::assemble(&absent, &fixture.options(), None, &Progress::none())
            .expect_err("no template, no config to generate");
        assert!(
            matches!(
                error,
                AuditError::MissingPackageInput {
                    what: "engagement config template",
                    ..
                }
            ),
            "{error}"
        );
        assert!(fixture.out_entries().is_empty(), "left a partial package");
    }

    #[test]
    fn a_blank_credential_is_refused_and_leaves_no_file() {
        let fixture = Fixture::new(BLANK_TEMPLATE);

        let error = fixture
            .assemble()
            .expect_err("a package with no key cannot run an audit");
        assert!(
            matches!(error, AuditError::MissingCredential { .. }),
            "{error}"
        );
        assert!(fixture.out_entries().is_empty(), "left a partial package");
    }

    #[test]
    fn a_blank_supplied_key_falls_back_to_the_template() {
        let fixture = Fixture::new(TEMPLATE);
        let blank = SecretKey::new("   ");

        let package = fixture.assemble_with(Some(&blank)).expect("assembles");
        assert!(!package.key_from_environment);
        let text = member(&package.path, "trusty-audit/engagement.toml");
        assert!(text.contains("sk-or-v1-template-key"), "{text}");
    }

    #[test]
    fn an_existing_package_is_never_overwritten() {
        let fixture = Fixture::new(TEMPLATE);
        std::fs::create_dir_all(&fixture.out).expect("out dir");
        let destination = fixture.out.join(PACKAGE_FILE_NAME);
        std::fs::write(&destination, b"the package already sent to the client").expect("sentinel");

        let error = fixture
            .assemble()
            .expect_err("regenerating must not destroy what was sent");
        assert!(matches!(error, AuditError::PackageExists { .. }), "{error}");
        assert_eq!(
            std::fs::read(&destination).expect("sentinel survives"),
            b"the package already sent to the client",
        );
        assert_eq!(fixture.out_entries(), vec![PACKAGE_FILE_NAME.to_owned()]);
    }

    /// The fail-open guard: a failure AFTER the temporary exists must remove it.
    ///
    /// Every other refusal here happens before a byte is written, so only this
    /// one exercises the cleanup. Deleting the `remove_file` call in
    /// [`write_archive`] leaves a `trusty-audit-install.zip.part` behind and
    /// fails this assertion.
    #[cfg(unix)]
    #[test]
    fn a_binary_that_cannot_be_read_leaves_no_partial_package() {
        use std::os::unix::fs::PermissionsExt as _;

        let fixture = Fixture::new(TEMPLATE);
        std::fs::set_permissions(&fixture.binary, std::fs::Permissions::from_mode(0o000))
            .expect("make the binary unreadable");
        // Root reads a 0o000 file regardless, which would make the fixture
        // prove nothing. Checking the precondition is not skipping coverage —
        // the guard is simply not observable through this fixture as root.
        if std::fs::File::open(&fixture.binary).is_ok() {
            return;
        }

        let error = fixture
            .assemble()
            .expect_err("an unreadable binary cannot be packaged");
        assert!(matches!(error, AuditError::Distribute { .. }), "{error}");
        assert!(
            fixture.out_entries().is_empty(),
            "a refused assembly left {:?} behind",
            fixture.out_entries()
        );
    }

    /// #5825: a failure mid-archive names the member that failed.
    ///
    /// The handler in [`assemble`] used to report [`BINARY_NAME`] for every
    /// failure, so a README that could not be written surfaced to the operator
    /// as "taudit failed" — pointing at the one member that had already
    /// succeeded. Restoring that handler makes the last row of this assertion
    /// read `("taudit", "failed")` while `taudit` is also still `ok`.
    #[test]
    fn a_failure_writing_a_member_names_that_member() {
        let fixture = Fixture::new(TEMPLATE);
        let (recorder, progress) = Recorder::new();

        let error = write_members(
            FailAtMember {
                written: std::io::Cursor::new(Vec::new()),
                marker: entry_path(README_NAME).into_bytes(),
            },
            &fixture.out.join("trusty-audit-install.zip.part"),
            &fixture.binary,
            Members {
                config: "openrouter_key = \"sk-or-v1-x\"\n".to_owned(),
                launcher: "#!/bin/sh\n".to_owned(),
                readme: "# Audit client\n".to_owned(),
                instructions: "# Running the audit\n".to_owned(),
                engagement_template: "openrouter_key = \"\"\n".to_owned(),
            },
            &progress,
        )
        .expect_err("the writer refused the README");
        assert!(matches!(error, AuditError::Distribute { .. }), "{error}");

        assert_eq!(
            unit_verdicts(&recorder),
            vec![
                (BINARY_NAME.to_owned(), "ok"),
                (LAUNCHER_NAME.to_owned(), "ok"),
                (EngagementConfig::FILE_NAME.to_owned(), "ok"),
                (README_NAME.to_owned(), "failed"),
            ],
        );
    }

    /// #5825: the binary's own failure still names the binary.
    ///
    /// The member the old handler always named is also a member that really can
    /// fail, so naming it correctly is what makes the README case above a
    /// distinction rather than a rename. The operation is reported as `0 of 4`
    /// either way — a refused assembly produces no package, whichever member
    /// stopped it.
    #[cfg(unix)]
    #[test]
    fn a_binary_that_cannot_be_read_names_the_binary() {
        use std::os::unix::fs::PermissionsExt as _;

        let fixture = Fixture::new(TEMPLATE);
        std::fs::set_permissions(&fixture.binary, std::fs::Permissions::from_mode(0o000))
            .expect("make the binary unreadable");
        // Root reads a 0o000 file regardless. See
        // `a_binary_that_cannot_be_read_leaves_no_partial_package`.
        if std::fs::File::open(&fixture.binary).is_ok() {
            return;
        }
        let (recorder, progress) = Recorder::new();

        super::assemble(&fixture.template, &fixture.options(), None, &progress)
            .expect_err("an unreadable binary cannot be packaged");

        assert_eq!(
            unit_verdicts(&recorder),
            vec![(BINARY_NAME.to_owned(), "failed")],
        );
        assert_eq!(operation_verdict(&recorder), Some((0, MEMBER_COUNT)));
    }

    #[test]
    fn a_supplied_key_beats_the_templates() {
        let fixture = Fixture::new(TEMPLATE);
        let supplied = SecretKey::new("sk-or-v1-from-the-environment");

        let package = fixture.assemble_with(Some(&supplied)).expect("assembles");
        assert!(package.key_from_environment);
        let text = member(&package.path, "trusty-audit/engagement.toml");
        assert!(text.contains("sk-or-v1-from-the-environment"), "{text}");
        assert!(!text.contains("sk-or-v1-template-key"), "{text}");
    }

    /// A template with a blank placeholder plus a supplied key is the shape the
    /// packaging flow is actually meant to run in: the real credential never
    /// lands on the auditor's disk.
    #[test]
    fn a_blank_template_plus_a_supplied_key_produces_a_runnable_config() {
        let fixture = Fixture::new(BLANK_TEMPLATE);
        let supplied = SecretKey::new("sk-or-v1-per-engagement");

        let package = fixture.assemble_with(Some(&supplied)).expect("assembles");
        let text = member(&package.path, "trusty-audit/engagement.toml");
        let loaded =
            EngagementConfig::from_toml(&text, Path::new("engagement.toml")).expect("loads");
        assert_eq!(loaded.openrouter_key.expose(), "sk-or-v1-per-engagement");
    }

    /// 🔴 #5483: the package the owner hands over carries NO credential.
    ///
    /// The key reaches the recipient out of band and the config carries a blank
    /// field, which is the exact state
    /// [`crate::cli::credential::resolve`] turns into the first-run terminal
    /// prompt. Scanning the whole member for `sk-or-` rather than checking the
    /// parsed field is deliberate: a key that leaked into `instructions`, a
    /// board credential, or a comment would pass the field check and still ship.
    #[test]
    fn a_prompt_for_key_package_ships_no_credential() {
        let fixture = Fixture::new(TEMPLATE);
        let mut options = fixture.options();
        options.prompt_for_key = true;

        let package = super::assemble(&fixture.template, &options, None, &Progress::none())
            .expect("assembles without a key");
        assert!(package.prompts_for_key);
        assert!(!package.key_from_environment);

        for entry in [
            EngagementConfig::FILE_NAME,
            README_NAME,
            instructions::INSTRUCTIONS_README,
            instructions::ENGAGEMENT_TEMPLATE_NAME,
        ] {
            let text = member(&package.path, &entry_path(entry));
            assert!(
                !text.contains("sk-or-"),
                "{entry} carries a credential: {text}"
            );
        }

        // The blank field is what selects the prompt, so prove the round trip
        // rather than only the absence.
        let text = member(&package.path, &entry_path(EngagementConfig::FILE_NAME));
        let loaded =
            EngagementConfig::from_toml(&text, Path::new("engagement.toml")).expect("loads");
        assert!(loaded.openrouter_key.is_empty());
        let resolved = crate::cli::credential::resolve(
            None,
            Some(&loaded.openrouter_key),
            Path::new("engagement.toml"),
            Some(&mut FakeTty::answering("sk-or-v1-typed-by-the-recipient")),
        )
        .expect("the recipient is asked");
        assert_eq!(
            resolved.source(),
            crate::cli::credential::CredentialSource::Prompt
        );
    }

    /// The flag beats a key in the environment, because it is the operator
    /// saying not to ship one — an `OPENROUTER_API_KEY` left over from an
    /// earlier command must not silently bake a credential into a package
    /// built to travel without one.
    #[test]
    fn prompting_for_the_key_beats_a_supplied_one() {
        let fixture = Fixture::new(TEMPLATE);
        let mut options = fixture.options();
        options.prompt_for_key = true;
        let supplied = SecretKey::new("sk-or-v1-from-the-environment");

        let package = super::assemble(
            &fixture.template,
            &options,
            Some(&supplied),
            &Progress::none(),
        )
        .expect("assembles");

        let text = member(&package.path, &entry_path(EngagementConfig::FILE_NAME));
        assert!(!text.contains("sk-or-v1-from-the-environment"), "{text}");
        assert!(!text.contains("sk-or-v1-template-key"), "{text}");
    }

    /// The instructions must set the right expectation for the package shape —
    /// telling a recipient with a baked-in key to wait for a prompt that never
    /// comes is the failure this covers.
    #[test]
    fn a_prompt_for_key_package_tells_the_recipient_to_expect_the_prompt() {
        // Two fixtures: `assemble` refuses to overwrite a package, so both
        // shapes cannot be built into one output directory.
        let asking = Fixture::new(TEMPLATE);
        let mut options = asking.options();
        options.prompt_for_key = true;
        let asks = super::assemble(&asking.template, &options, None, &Progress::none())
            .expect("assembles");
        let asks = instructions_of(&asks);
        assert!(asks.contains("ask for the OpenRouter key"), "{asks}");

        let fixture = Fixture::new(TEMPLATE);
        let baked = instructions_of(&fixture.assemble().expect("assembles"));
        assert!(baked.contains("already in `../engagement.toml`"), "{baked}");
        assert!(!baked.contains("ask for the OpenRouter key"), "{baked}");
    }

    /// #5483: `--template cast` writes the two keys that make the output a
    /// CAST-style report (#6669), so an auditor no longer hand-edits the
    /// template before every handoff.
    #[test]
    fn the_cast_preset_writes_both_report_keys() {
        let fixture = Fixture::new(TEMPLATE);
        let mut options = fixture.options();
        options.report_preset = ReportPreset::Cast;

        let package = super::assemble(&fixture.template, &options, None, &Progress::none())
            .expect("assembles");

        let text = member(&package.path, &entry_path(EngagementConfig::FILE_NAME));
        let loaded =
            EngagementConfig::from_toml(&text, Path::new("engagement.toml")).expect("loads");
        assert_eq!(loaded.report.template.as_deref(), Some("cast"));
        assert_eq!(loaded.report.code_only, Some(true));
        // The pair the sweep hands down to `trusty-review report`.
        assert_eq!(loaded.report.child_env().len(), 2);
    }

    /// The default preset must change nothing, or every existing engagement's
    /// package silently switches report.
    #[test]
    fn the_default_preset_leaves_the_templates_report_table_alone() {
        let fixture = Fixture::new(TEMPLATE);
        let package = fixture.assemble().expect("assembles");

        let text = member(&package.path, &entry_path(EngagementConfig::FILE_NAME));
        let loaded =
            EngagementConfig::from_toml(&text, Path::new("engagement.toml")).expect("loads");
        assert_eq!(loaded.report.template, None);
        assert_eq!(loaded.report.code_only, None);
        assert!(loaded.report.child_env().is_empty());
    }

    /// 🔴 The acceptance case, end to end: a CAST package with no key and a
    /// pre-populated repository list, unzipped and inspected.
    ///
    /// This is the shape the handover actually ships, so it is asserted as one
    /// package rather than as three independent properties that could each hold
    /// while the combination does not.
    #[test]
    fn a_cast_package_with_a_repo_list_and_no_key_carries_all_three() {
        let fixture = Fixture::new(TEMPLATE);
        let mut options = fixture.options();
        options.prompt_for_key = true;
        options.report_preset = ReportPreset::Cast;
        let list = fixture.template.with_file_name("repos.txt");
        std::fs::write(
            &list,
            "# the auditor's list\nacme/api\nhttps://github.com/acme/web.git\n",
        )
        .expect("write repos.txt");
        options.repos = Some(list);

        let package = super::assemble(&fixture.template, &options, None, &Progress::none())
            .expect("assembles");
        assert_eq!(package.declared_repos, 2);

        // Both instruction members are present.
        for entry in [
            instructions::INSTRUCTIONS_README,
            instructions::ENGAGEMENT_TEMPLATE_NAME,
        ] {
            assert!(
                entries(&package.path).contains(&entry_path(entry)),
                "{entry} is missing from {:?}",
                entries(&package.path)
            );
        }

        // The root config: CAST keys, the declared repositories, no credential.
        let text = member(&package.path, &entry_path(EngagementConfig::FILE_NAME));
        assert!(!text.contains("sk-or-"), "{text}");
        let loaded =
            EngagementConfig::from_toml(&text, Path::new("engagement.toml")).expect("loads");
        assert_eq!(loaded.report.template.as_deref(), Some("cast"));
        assert_eq!(loaded.report.code_only, Some(true));
        assert!(loaded.openrouter_key.is_empty());
        assert_eq!(
            loaded
                .declared_targets()
                .expect("targets are declared")
                .iter()
                .map(Target::id)
                .collect::<Vec<_>>(),
            vec!["acme/api", "acme/web"],
        );

        // `taudit audit` reads exactly that list, with no picker — the property
        // the instructions promise.
        assert_eq!(
            crate::registry::engagement_targets(
                Some(&loaded),
                &crate::workdir::WorkDir::new(fixture.out.join("work"))
            )
            .expect("reads the declared list")
            .len(),
            2,
        );

        // The launcher is executable, and the config is not.
        #[cfg(unix)]
        assert_eq!(mode_of(&package.path, LAUNCHER_NAME), Some(0o755));
        #[cfg(unix)]
        assert_eq!(
            mode_of(&package.path, EngagementConfig::FILE_NAME),
            Some(0o600)
        );

        let readme = instructions_of(&package);
        assert!(readme.contains("2 repositories"), "{readme}");
        assert!(
            !readme.contains("register each repository"),
            "the instructions still send the recipient to the picker: {readme}"
        );
    }

    /// A package with no list keeps sending the recipient to the picker, which
    /// is the fallback rather than the default.
    #[test]
    fn a_package_with_a_repo_list_says_the_targets_are_already_declared() {
        let fixture = Fixture::new(TEMPLATE);
        let readme = instructions_of(&fixture.assemble().expect("assembles"));
        assert!(readme.contains("register each repository"), "{readme}");
        assert!(readme.contains("./audit.sh add repo"), "{readme}");
    }

    #[test]
    fn the_default_output_directory_is_under_home() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let path = destination(&DistributeOptions::default()).expect("resolves");
        assert_eq!(
            path,
            home.join("duetto").join("audit").join(PACKAGE_FILE_NAME)
        );
    }

    #[test]
    fn a_chosen_directory_beats_the_default() {
        let options = DistributeOptions {
            output_dir: Some(PathBuf::from("/tmp/elsewhere")),
            binary: None,
            ..DistributeOptions::default()
        };
        assert_eq!(
            destination(&options).expect("resolves"),
            PathBuf::from("/tmp/elsewhere").join(PACKAGE_FILE_NAME),
        );
    }

    #[test]
    fn expanding_a_tilde_reaches_the_home_directory() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        assert_eq!(
            expand_home(Path::new("~/duetto/audit")).expect("expands"),
            home.join("duetto").join("audit"),
        );
        assert_eq!(expand_home(Path::new("~")).expect("expands"), home);
    }

    #[test]
    fn a_path_without_a_tilde_is_unchanged() {
        for raw in ["/absolute/path", "relative/path", "./here"] {
            assert_eq!(
                expand_home(Path::new(raw)).expect("expands"),
                PathBuf::from(raw),
                "{raw}"
            );
        }
    }

    #[test]
    fn another_users_tilde_is_refused_rather_than_taken_literally() {
        let error = expand_home(Path::new("~someone/audit"))
            .expect_err("a passwd lookup is not this crate's business");
        assert!(
            matches!(error, AuditError::MissingPackageInput { .. }),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_binary_and_launcher_extract_executable_and_the_config_does_not() {
        let fixture = Fixture::new(TEMPLATE);
        let package = fixture.assemble().expect("assembles");
        let file = std::fs::File::open(&package.path).expect("open package");
        let mut archive = zip::ZipArchive::new(file).expect("read package");

        for (entry, mode) in [
            ("trusty-audit/taudit", 0o755),
            ("trusty-audit/audit.sh", 0o755),
            ("trusty-audit/engagement.toml", 0o600),
        ] {
            let member = archive.by_name(entry).expect("member present");
            // `unix_mode` carries the file-type bits too (0o100755, not 0o755).
            assert_eq!(
                member.unix_mode().map(|m| m & 0o777),
                Some(mode),
                "{entry} extracts with the wrong permissions"
            );
        }
    }
}
