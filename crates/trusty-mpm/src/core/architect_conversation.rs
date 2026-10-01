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
//! directory's project folder of the Architect's config dir. Any other
//! answer starts fresh and names the reason.
//! Test: `architect_conversation_tests.rs`; `tests/tm_fleet.rs`
//! (`fleet_init_resumes_the_prior_conversation_after_the_record_is_gone`).

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
/// start. A record that does not read or parse, names another directory, holds
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
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return fresh(None),
        Err(e) => return fresh(Some(format!("{} could not be read: {e}", path.display()))),
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
