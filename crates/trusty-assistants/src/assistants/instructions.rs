//! Reading and role-seeding an assistant instance's `instructions.md` (#11649).
//!
//! Why: [`super::home::AssistantHome::ensure`] seeds `instructions.md`, but
//! nothing read it, so every instance ran on the shared persona alone. Slice 2
//! gives each instance its own text: a role template written once at
//! provisioning, and a reader that slice 3 splices into the turn prompt.
//! What: [`AssistantRole`] names the two shipped roles; [`seed_role_instructions`]
//! writes the role template only when the file is absent;
//! [`AssistantHome::read_instructions`] returns the text, bounded by
//! [`MAX_INSTRUCTIONS_BYTES`]. Every failure is a typed [`AssistantError`]:
//! the reader never returns an empty string in place of an error and never
//! rewrites the file.
//! Test: `super::tests::instructions_tests` — the whole module.

use std::io::{ErrorKind, Read, Write};
use std::path::Path;

use super::error::AssistantError;
use super::home::AssistantHome;

/// Largest `instructions.md` the reader accepts, in bytes.
///
/// Why: the text is spliced into every turn prompt (slice 3), so an unbounded
/// file is an unbounded prompt. 64 KiB is far above any hand-written file.
/// Test: `super::tests::instructions_tests::an_oversized_file_is_a_typed_error`.
pub const MAX_INSTRUCTIONS_BYTES: u64 = 64 * 1024;

/// The role an assistant instance is provisioned for.
///
/// Why: the role picks the instructions template; it is not stored, because
/// the instance's own `instructions.md` is the record after seeding.
/// What: `personal` or `cto`, parsed by [`AssistantRole::parse`].
/// Test: `super::tests::instructions_tests::roles_parse_and_reject_unknown_names`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistantRole {
    /// The owner's personal assistant.
    Personal,
    /// The CTO's assistant.
    Cto,
}

impl AssistantRole {
    /// Parse a role name. Anything other than `personal` or `cto` is
    /// [`AssistantError::InvalidRole`], never a default role.
    /// Test: `super::tests::instructions_tests::roles_parse_and_reject_unknown_names`.
    pub fn parse(raw: &str) -> Result<Self, AssistantError> {
        match raw.trim() {
            "personal" => Ok(Self::Personal),
            "cto" => Ok(Self::Cto),
            _ => Err(AssistantError::InvalidRole {
                raw: raw.to_string(),
            }),
        }
    }

    /// The role's name as [`Self::parse`] accepts it.
    /// Test: `super::tests::instructions_tests::roles_parse_and_reject_unknown_names`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Cto => "cto",
        }
    }

    /// The role's `instructions.md` template, distilled from the Architect's
    /// operating manual.
    /// Test: `super::tests::instructions_tests::seeding_writes_the_role_template_once`.
    pub fn template(self) -> &'static str {
        match self {
            Self::Personal => include_str!("templates/personal.md"),
            Self::Cto => include_str!("templates/cto.md"),
        }
    }
}

/// Write `role`'s template to `home`'s `instructions.md` when no file is there.
///
/// Why: the file is the user's after it exists; provisioning must never
/// replace an edited one, including on a second run.
/// What: creates the home directory if needed, then writes through
/// [`write_new`], so an existing file is never truncated even when two callers
/// race, and a failed write leaves no partial file. Returns `true` when this call wrote the file, `false` when one was
/// already there.
/// Test: `super::tests::instructions_tests::seeding_writes_the_role_template_once`,
/// `super::tests::instructions_tests::seeding_never_overwrites_an_edited_file`.
pub fn seed_role_instructions(
    home: &AssistantHome,
    role: AssistantRole,
) -> Result<bool, AssistantError> {
    std::fs::create_dir_all(home.path()).map_err(|source| AssistantError::Io {
        path: home.path().to_path_buf(),
        source,
    })?;
    write_new(&home.instructions_path(), role.template().as_bytes())
}

/// Write `body` to `path` only when nothing is there; `Ok(false)` if it exists.
///
/// Why: a failed write must not leave a truncated `instructions.md`,
/// `persona.md` or `agent.toml` that every later run then keeps (#11649 review).
/// What: writes a hidden temp file in the same directory, then `hard_link`s it
/// to `path` — atomic, and `AlreadyExists` when a file is there. The temp file
/// is removed on every path out.
/// Test: `super::tests::instructions_tests::a_failed_write_leaves_no_partial_file`.
pub(super) fn write_new(path: &Path, body: &[u8]) -> Result<bool, AssistantError> {
    write_new_via(path, body, |file, body| file.write_all(body))
}

/// [`write_new`] with the body writer injectable, so the failure arm is testable.
pub(super) fn write_new_via(
    path: &Path,
    body: &[u8],
    write: impl FnOnce(&mut std::fs::File, &[u8]) -> std::io::Result<()>,
) -> Result<bool, AssistantError> {
    if path.exists() {
        return Ok(false);
    }
    let io = |at: &Path, source| AssistantError::Io {
        path: at.to_path_buf(),
        source,
    };
    let tmp = temp_sibling(path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| io(&tmp, e))?;
    let written = write(&mut file, body).and_then(|()| file.sync_all());
    drop(file);
    let linked = written.and_then(|()| std::fs::hard_link(&tmp, path));
    // Best effort: the temp name is unique, and a leftover is never read.
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(io(path, e)),
    }
}

/// A unique hidden temp path beside `path`.
fn temp_sibling(path: &Path) -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()))
}

impl AssistantHome {
    /// This instance's `instructions.md` text.
    ///
    /// Why: slice 3 splices it into the turn prompt, so a broken file must be
    /// reported, never replaced by nothing — an assistant running with no
    /// instructions looks healthy and is not.
    /// What: reads at most [`MAX_INSTRUCTIONS_BYTES`] + 1 bytes. Errors:
    /// [`AssistantError::InstructionsMissing`] (absent),
    /// [`AssistantError::InstructionsUnreadable`] (not a file, permissions, I/O),
    /// [`AssistantError::InstructionsTooLarge`], [`AssistantError::InstructionsNotUtf8`]
    /// and [`AssistantError::InstructionsEmpty`] (whitespace only). Never writes.
    /// Test: `super::tests::instructions_tests::reads_the_seeded_text`,
    /// `super::tests::instructions_tests::a_missing_file_is_a_typed_error`,
    /// `super::tests::instructions_tests::a_non_utf8_file_is_a_typed_error`,
    /// `super::tests::instructions_tests::an_oversized_file_is_a_typed_error`,
    /// `super::tests::instructions_tests::an_unreadable_file_is_a_typed_error`,
    /// `super::tests::instructions_tests::an_empty_file_is_a_typed_error`.
    pub fn read_instructions(&self) -> Result<String, AssistantError> {
        let path = self.instructions_path();
        let unreadable = |source| AssistantError::InstructionsUnreadable {
            path: path.clone(),
            source,
        };
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(AssistantError::InstructionsMissing { path });
            }
            Err(e) => return Err(unreadable(e)),
        };
        let meta = file.metadata().map_err(unreadable)?;
        if !meta.is_file() {
            return Err(unreadable(std::io::Error::new(
                ErrorKind::InvalidInput,
                "it is not a regular file",
            )));
        }
        let mut bytes = Vec::new();
        file.take(MAX_INSTRUCTIONS_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(unreadable)?;
        if bytes.len() as u64 > MAX_INSTRUCTIONS_BYTES {
            return Err(AssistantError::InstructionsTooLarge {
                path,
                cap: MAX_INSTRUCTIONS_BYTES,
            });
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| AssistantError::InstructionsNotUtf8 { path: path.clone() })?;
        if text.trim().is_empty() {
            return Err(AssistantError::InstructionsEmpty { path });
        }
        Ok(text)
    }
}
