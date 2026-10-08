//! The Architect's Claude conversation id, kept across relaunches (#8981).
//!
//! Why: `tm fleet init` started every Architect on a fresh conversation. The
//! only copy of the conversation id was the daemon's session record, and a
//! deleted or tombstoned record (a8a726e2, 2026-10-01) lost it. The Architect
//! directory holds many other `claude` transcripts, so "the newest transcript"
//! would resume some other session's conversation.
//! What: a fresh launch names its own id (`claude --session-id <uuid>`) and
//! [`record_conversation`] writes it to `last.architect-conversation` in the
//! sealed `~/.trusty-mpm/architect-launch/` directory, beside the launch
//! records. [`resolve_conversation`] reads it back at the next launch and
//! resumes it (`claude --resume <uuid>`) only when the id is a UUID recorded
//! for this directory and Claude Code holds its transcript in this
//! directory's project folder of the Architect's config dir, and the record
//! is a regular file, not a symlink, that this user owns and no other user
//! can write. Any other answer starts fresh and names
//! the reason. A resume whose `claude` does not stay up is a failed resume:
//! the launch removes the record with [`clear_conversation`], so the next
//! launch starts fresh instead of retrying a dead conversation.
//! Test: `architect_conversation_tests.rs`; `tests/tm_fleet.rs`
//! (`fleet_init_resumes_the_prior_conversation_after_the_record_is_gone`,
//! `fleet_init_clears_the_conversation_record_when_the_resume_fails`).

use std::path::{Path, PathBuf};

use crate::core::architect_launch::ARCHITECT_DIR;
use crate::core::architect_session::write_owner_only;

/// Extension of the conversation record; pm-guard denies an unplaceable
/// write of this name, as it does for the launch records.
pub const CONVERSATION_EXT: &str = "architect-conversation";

/// The conversation record's file name under the launch directory.
pub const CONVERSATION_FILE: &str = "last.architect-conversation";

/// The record's content: the Architect directory and its conversation id.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationRecord {
    project_dir: PathBuf,
    conversation_id: String,
}

/// How the next Architect launch starts its conversation (#8981).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationStart {
    /// Resume the recorded conversation.
    Resume(String),
    /// Start a new conversation with this id. `reason` says why a recorded
    /// conversation was not resumed; `None` when none was recorded.
    Fresh {
        /// The new conversation's id.
        id: String,
        /// Why the recorded conversation was set aside.
        reason: Option<String>,
    },
}

impl ConversationStart {
    /// The conversation id this launch runs under.
    pub fn id(&self) -> &str {
        match self {
            Self::Resume(id) | Self::Fresh { id, .. } => id,
        }
    }

    /// The `claude` arguments that select the conversation.
    ///
    /// Test: `the_claude_args_name_the_conversation`.
    pub fn claude_args(&self) -> [String; 2] {
        match self {
            Self::Resume(id) => ["--resume".to_owned(), id.clone()],
            Self::Fresh { id, .. } => ["--session-id".to_owned(), id.clone()],
        }
    }

    /// One line for `tm fleet init`'s summary.
    pub fn describe(&self) -> String {
        match self {
            Self::Resume(id) => format!("resuming conversation {id}"),
            Self::Fresh { id, reason: None } => format!("new conversation {id}"),
            Self::Fresh {
                id,
                reason: Some(why),
            } => format!("new conversation {id} (the prior one was not resumed: {why})"),
        }
    }
}

/// Path of the conversation record under `root`, the `~/.trusty-mpm` directory.
pub fn conversation_path(root: &Path) -> PathBuf {
    root.join(ARCHITECT_DIR).join(CONVERSATION_FILE)
}

/// Claude Code's project-folder name for `dir`: every character that is not
/// an ASCII letter or digit becomes `-`.
///
/// Test: `the_project_folder_name_matches_claude_code`.
pub fn project_folder(dir: &Path) -> String {
    dir.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Whether `config_dir` holds transcript `<id>.jsonl` for a conversation that
/// ran in `dir`, spelled as given or canonicalized.
fn transcript_exists(config_dir: &Path, dir: &Path, id: &str) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(dir).ok();
    [Some(dir.to_path_buf()), canonical]
        .into_iter()
        .flatten()
        .map(|d| {
            config_dir
                .join("projects")
                .join(project_folder(&d))
                .join(format!("{id}.jsonl"))
        })
        .find(|path| path.is_file())
}

/// Whether `a` and `b` name the same directory, canonical when they can be.
fn same_dir(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

/// How the Architect launch in `project_dir` starts its conversation (#8981).
///
/// Why: resume the Architect's own conversation from a record that survives
/// the daemon's session record, and fail safe — never resume a conversation
/// tm cannot prove is the Architect's.
/// What: reads [`conversation_path`] under `root`. No record is a plain fresh
/// start. A record that does not read, is writable by other users, does not
/// parse, names another directory, holds
/// no valid UUID, or has no transcript under `config_dir` (the `claude`
/// config dir the launch sets; `None` means it cannot be checked) is a fresh
/// start whose reason says which. Only a record passing every check is
/// [`ConversationStart::Resume`]. A fresh start gets a new v4 UUID.
/// Test: `a_recorded_conversation_with_a_transcript_is_resumed`,
/// `every_unusable_record_starts_fresh_and_says_why`,
/// `no_record_starts_fresh_without_a_reason`.
pub fn resolve_conversation(
    root: &Path,
    project_dir: &Path,
    config_dir: Option<&Path>,
) -> ConversationStart {
    let fresh = |reason: Option<String>| ConversationStart::Fresh {
        id: uuid::Uuid::new_v4().to_string(),
        reason,
    };
    let path = conversation_path(root);
    let text = match read_owner_only(&path) {
        Ok(Some(text)) => text,
        Ok(None) => return fresh(None),
        Err(why) => return fresh(Some(why)),
    };
    let record: ConversationRecord = match serde_json::from_str(&text) {
        Ok(record) => record,
        Err(e) => return fresh(Some(format!("{} is corrupt: {e}", path.display()))),
    };
    if !same_dir(&record.project_dir, project_dir) {
        return fresh(Some(format!(
            "the recorded conversation belongs to {}",
            record.project_dir.display()
        )));
    }
    let id = record.conversation_id;
    // Only the canonical lowercase hyphenated form tm writes; it is also
    // the one form `claude --resume` cannot read as a flag or a search term.
    if uuid::Uuid::try_parse(&id).map(|u| u.to_string()) != Ok(id.clone()) {
        return fresh(Some(format!(
            "the recorded id {id:?} is not a valid conversation id"
        )));
    }
    let Some(config_dir) = config_dir else {
        return fresh(Some(
            "the Architect's claude config dir is unknown, so its transcript cannot be checked"
                .to_owned(),
        ));
    };
    if transcript_exists(config_dir, project_dir, &id).is_none() {
        return fresh(Some(format!(
            "conversation {id} has no transcript under {}",
            config_dir.join("projects").display()
        )));
    }
    ConversationStart::Resume(id)
}

/// The record's text, `None` when absent (#8981 critic LOW).
///
/// What: [`read_owned_by`] with this process's effective uid as the owner.
/// Test: `every_unusable_record_starts_fresh_and_says_why`.
fn read_owner_only(path: &Path) -> Result<Option<String>, String> {
    // SAFETY: `geteuid` takes no arguments, reads a process property and
    // cannot fail.
    read_owned_by(path, unsafe { libc::geteuid() })
}

/// The text of `path`, a regular file owned by `owner`; `None` when absent.
///
/// What: opened with `O_NOFOLLOW | O_NONBLOCK` (#8981 round 2), so neither a
/// symlink nor a FIFO is followed or blocks the open. The owner and mode are
/// read from the open handle the text is read from, so the bytes checked are
/// the bytes used. A non-file, a read failure, another owner, or a file group
/// or others can write (mode `& 0o022`) is `Err` naming the path.
/// Test: `every_unusable_record_starts_fresh_and_says_why`,
/// `a_record_owned_by_another_user_is_refused`.
fn read_owned_by(path: &Path, owner: u32) -> Result<Option<String>, String> {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
    let unreadable =
        |e: &dyn std::fmt::Display| format!("{} could not be read: {e}", path.display());
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path);
    let mut file = match opened {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(unreadable(&e)),
        Ok(file) => file,
    };
    let meta = file.metadata().map_err(|e| unreadable(&e))?;
    if !meta.is_file() {
        return Err(unreadable(&"not a regular file"));
    }
    if meta.uid() != owner {
        return Err(format!(
            "{} is owned by uid {}, not by this user (uid {owner})",
            path.display(),
            meta.uid()
        ));
    }
    let mode = meta.permissions().mode() & 0o7777;
    if mode & 0o022 != 0 {
        return Err(format!(
            "{} is writable by other users (mode {mode:o})",
            path.display()
        ));
    }
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|e| unreadable(&e))?;
    Ok(Some(text))
}

/// Remove the conversation record after a failed resume (#8981 critic HIGH).
///
/// What: deletes [`conversation_path`] under `root`; an absent record is `Ok`.
/// Test: `fleet_init_clears_the_conversation_record_when_the_resume_fails`.
pub fn clear_conversation(root: &Path) -> Result<(), String> {
    let path = conversation_path(root);
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {e}", path.display()))
        }
        _ => Ok(()),
    }
}

/// Record `id` as the Architect's conversation in `project_dir` (#8981).
///
/// What: writes [`conversation_path`] under `root`, mode 0600, atomically.
/// Test: `a_recorded_conversation_with_a_transcript_is_resumed`.
pub fn record_conversation(root: &Path, project_dir: &Path, id: &str) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(&ConversationRecord {
        project_dir: project_dir.to_path_buf(),
        conversation_id: id.to_owned(),
    })
    .map_err(|e| e.to_string())?;
    write_owner_only(&conversation_path(root), &body)
}

#[cfg(test)]
#[path = "architect_conversation_tests.rs"]
mod tests;
