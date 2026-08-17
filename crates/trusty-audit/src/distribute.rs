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
//! and a README naming the three commands in order. [`InstallPackage`] is what a
//! front end renders.
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
use crate::run::ENV_INFERENCE_CREDENTIAL;

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

/// Working directory the launcher pins, relative to the extracted root.
const WORK_DIR_NAME: &str = "work";

/// How much of the binary is read at a time while copying it.
const CHUNK_BYTES: usize = 64 * 1024;

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
}

/// The finished install package: what to send, and what it will run on.
///
/// Why: an operator about to hand a client a zip needs to know what is in it and
/// which machine it is for. `platform` is the field that makes the one failure
/// mode this package has visible before it ships rather than after: a binary
/// built for the wrong target extracts and runs on nothing.
/// What: the destination, every member, the sizes, and the host triple the
/// binary was built for.
/// Test: `super::distribute_tests::the_package_holds_the_four_members`.
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
    pub key_from_environment: bool,
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
/// Test: `super::distribute_tests::a_supplied_key_beats_the_templates`,
/// `super::distribute_tests::a_blank_supplied_key_falls_back_to_the_template`,
/// `super::distribute_tests::a_blank_credential_is_refused_and_leaves_no_file`.
fn resolve_key(
    supplied: Option<&SecretKey>,
    template: &EngagementConfig,
    template_path: &Path,
) -> Result<(SecretKey, bool), AuditError> {
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
    let (key, key_from_environment) = resolve_key(supplied_key, &template, template_path)?;
    let config_text = config::generate(&template_text, &key, template_path)?;

    let platform = host_platform();
    let readme = render_readme(&template, &platform, options.binary.is_some());
    let launcher = render_launcher();

    progress.operation_started(Operation::Distribute, 4);
    let result = write_archive(
        &destination,
        &binary,
        Members {
            config: config_text,
            launcher,
            readme,
        },
        progress,
    );
    match &result {
        Ok(_) => progress.operation_finished(Operation::Distribute, 4, 4),
        Err(e) => {
            progress.unit_finished(
                Operation::Distribute,
                BINARY_NAME,
                UnitOutcome::Failed(e.to_string()),
            );
            progress.operation_finished(Operation::Distribute, 0, 4);
        }
    }
    let (files, packaged_bytes) = result?;

    Ok(InstallPackage {
        path: destination,
        total_bytes: files.iter().map(|f| f.bytes).sum(),
        files,
        packaged_bytes,
        platform,
        key_from_environment,
    })
}

/// The three generated text members, kept together so [`write_archive`]'s
/// signature does not grow three `String` parameters a caller can transpose.
struct Members {
    config: String,
    launcher: String,
    readme: String,
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

type Archive = zip::ZipWriter<std::io::BufWriter<std::fs::File>>;

/// Write one zip entry's header.
fn start(zip: &mut Archive, entry: &str, mode: u32, at: &Path) -> Result<(), AuditError> {
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
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let mut files = Vec::with_capacity(4);

    progress.unit_started(Operation::Distribute, BINARY_NAME, 1, 4);
    let entry = entry_path(BINARY_NAME);
    let bytes = copy_binary(&mut zip, &entry, binary, temporary)?;
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
    ]
    .into_iter()
    .enumerate()
    {
        progress.unit_started(Operation::Distribute, name, index + 2, 4);
        let entry = entry_path(name);
        start(&mut zip, &entry, mode, temporary)?;
        zip.write_all(text.as_bytes())
            .map_err(|source| AuditError::Distribute {
                path: temporary.to_path_buf(),
                source,
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
fn copy_binary(
    zip: &mut Archive,
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
/// the one the shell sees. Pins `--work-dir` beside the launcher so the client's
/// `rm -rf` uninstall promise covers one predictable place.
/// Test: `super::distribute_tests::the_launcher_resolves_everything_beside_itself`.
fn render_launcher() -> String {
    format!(
        "#!/bin/sh\n\
         # Runs the audit client from this directory. No install step, no PATH.\n\
         set -eu\n\
         here=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\n\
         exec \"$here/{BINARY_NAME}\" \\\n\
         \x20 --config \"$here/{config}\" \\\n\
         \x20 --work-dir \"$here/{WORK_DIR_NAME}\" \\\n\
         \x20 \"$@\"\n",
        config = EngagementConfig::FILE_NAME,
    )
}

/// The README the recipient reads first.
///
/// Why: #5825 asks for the three commands in order and nothing else. The
/// quarantine paragraph is here because it is the difference between a package
/// that runs and one that produces "cannot be opened because the developer
/// cannot be verified" — Developer-ID signing (#5484) is a separate milestone,
/// and until it lands the recipient needs the one command that clears the flag.
/// Test: `super::distribute_tests::the_readme_names_the_three_commands_in_order`.
fn render_readme(config: &EngagementConfig, platform: &str, explicit_binary: bool) -> String {
    let engagement = match (&config.client, &config.engagement) {
        (Some(client), Some(label)) => format!("{client} — {label}"),
        (Some(client), None) => client.clone(),
        (None, Some(label)) => label.clone(),
        (None, None) => "unlabelled".to_owned(),
    };
    let platform_line = if explicit_binary {
        format!("- Built for: {platform} (binary supplied by the auditor)")
    } else {
        format!("- Built for: {platform}")
    };
    format!(
        "# Audit client — {engagement}\n\
         \n\
         Extract this folder anywhere and run the three commands below from inside it.\n\
         Nothing is installed on your machine outside this folder: deleting the folder\n\
         removes the client, its tooling, and everything it wrote.\n\
         \n\
         {platform_line}\n\
         - Configuration: `{config_file}` (readable — open it before you run anything)\n\
         - Working directory: `{work}/`, created beside this file on first run\n\
         \n\
         ## 1. Register what to audit\n\
         \n\
         ```sh\n\
         ./{launcher} add repo <owner>/<name>\n\
         ./{launcher} targets\n\
         ```\n\
         \n\
         Each repository is checked against your own GitHub credential before it is\n\
         registered, so a typo is refused here rather than halfway through the audit.\n\
         Repeat for every repository. `targets` lists what is registered.\n\
         \n\
         ## 2. Run the audit\n\
         \n\
         ```sh\n\
         ./{launcher} run\n\
         ```\n\
         \n\
         This clones the registered repositories and analyses them. It may run for\n\
         hours and prints progress as it goes. A repository that fails does not stop\n\
         the rest, and a re-run resumes rather than starting over.\n\
         \n\
         ## 3. Return the results\n\
         \n\
         ```sh\n\
         ./{launcher} package\n\
         ```\n\
         \n\
         This writes one zip and prints its path. Open it before you send it — it is\n\
         unencrypted on purpose, so you can see exactly what leaves your network. It\n\
         carries reports and metadata, and it never carries the credential in\n\
         `{config_file}`.\n\
         \n\
         ## If macOS refuses to run it\n\
         \n\
         A zip that arrived over the network is quarantined, and this build is not yet\n\
         notarized. Clear the flag once, from inside the extracted folder:\n\
         \n\
         ```sh\n\
         xattr -dr com.apple.quarantine .\n\
         ```\n",
        config_file = EngagementConfig::FILE_NAME,
        launcher = LAUNCHER_NAME,
        work = WORK_DIR_NAME,
    )
}

#[cfg(test)]
mod distribute_tests {
    use super::*;

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

    /// Every entry name in a written package, in order.
    fn entries(zip_path: &Path) -> Vec<String> {
        let file = std::fs::File::open(zip_path).expect("open package");
        let mut archive = zip::ZipArchive::new(file).expect("read package");
        (0..archive.len())
            .map(|i| archive.by_index(i).expect("entry").name().to_owned())
            .collect()
    }

    #[test]
    fn the_package_holds_the_four_members() {
        let fixture = Fixture::new(TEMPLATE);
        let package = fixture.assemble().expect("assembles");

        assert_eq!(
            entries(&package.path),
            vec![
                "trusty-audit/taudit",
                "trusty-audit/audit.sh",
                "trusty-audit/engagement.toml",
                "trusty-audit/README.md",
            ],
        );
        assert_eq!(package.files.len(), 4);
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
        assert!(script.contains("--work-dir \"$here/work\""), "{script}");
        // The binary is never named bare: every occurrence is `$here/`-prefixed,
        // so a `taudit` already on PATH at some other version cannot win.
        for (index, _) in script.match_indices(BINARY_NAME) {
            assert!(
                script[..index].ends_with("$here/"),
                "the launcher must never reach for an installed taudit: {script}"
            );
        }
    }

    #[test]
    fn the_readme_names_the_three_commands_in_order() {
        let fixture = Fixture::new(TEMPLATE);
        let package = fixture.assemble().expect("assembles");

        let readme = member(&package.path, "trusty-audit/README.md");
        let add = readme
            .find("./audit.sh add repo")
            .expect("registers targets");
        let run = readme.find("./audit.sh run").expect("runs the audit");
        let ret = readme
            .find("./audit.sh package")
            .expect("returns the package");
        assert!(
            add < run && run < ret,
            "the three commands are out of order"
        );
        assert!(readme.contains("Acme — 2026-Q3"), "{readme}");
        assert!(readme.contains("com.apple.quarantine"), "{readme}");
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
