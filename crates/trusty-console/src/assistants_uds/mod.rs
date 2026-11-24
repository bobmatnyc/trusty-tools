//! trusty-console's read side of trusty-assistants (#11747, epic #11746).
//!
//! Why: the Trusty Assistants web console lists instances and shows their turn
//! history. trusty-assistants (`ta`) owns both facts on disk, and its per-instance
//! ask daemon owns the answer path. The console has no Cargo edge on
//! trusty-assistants (ruling Df0), so every rule it needs is mirrored here and
//! pinned to its source by the contract tests in `tests.rs`.
//!
//! What:
//! - the assistants root and the instance rule `ta list` applies
//!   ([`assistants_root`], [`list_instances`], [`InstanceId`]);
//! - the ask daemon's wire structs and socket path ([`AskRequest`],
//!   [`AskReply`], [`PROTOCOL_VERSION`], [`socket_path_in`]);
//! - `ask` and `ask_route`, the #11748 ask client and
//!   `POST /api/console/assistants/{id}/ask`, with the guards on every route;
//! - `turns`, the tolerant tail read of `<home>/stores/ask-turns.jsonl`;
//! - `routes`, the two `GET /api/console/assistants…` handlers.
//!
//! Test: `tests.rs` (rules and contract) and `tests/assistants_routes.rs`
//! (the routes through the real router).

pub mod ask;
pub mod ask_route;
pub mod routes;
pub mod turns;

#[cfg(test)]
mod ask_tests;
#[cfg(all(test, unix))]
mod gate_tests;
#[cfg(test)]
mod tests;

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ─── mirrored from trusty-assistants ─────────────────────────────────────────
//
// Literals, not imports (ruling Df0). `tests::the_mirrored_constants_match_the_source`
// reads each declaration back out of trusty-assistants and fails when one moves.

/// The ask wire protocol version. Mirrors
/// `crates/trusty-assistants/src/ask/daemon/mod.rs` `PROTOCOL_VERSION`.
/// Test: `tests::the_mirrored_constants_match_the_source`.
pub const PROTOCOL_VERSION: u32 = 1;

/// The assistants-root override `ta` reads first (`assistants/home.rs`).
/// Test: `tests::the_mirrored_constants_match_the_source`.
pub const ASSISTANTS_DIR_ENV: &str = "TA_ASSISTANTS_DIR";

/// The pre-rename override `ta` reads second (`assistants/home.rs`).
/// Test: `tests::the_mirrored_constants_match_the_source`.
pub const LEGACY_ASSISTANTS_DIR_ENV: &str = "TAGENT_ASSISTANTS_DIR";

/// The dotless directory under the user's home holding one home per instance.
/// Test: `tests::the_mirrored_constants_match_the_source`.
pub const ASSISTANTS_DIR_NAME: &str = "trusty-agents";

/// The file that makes a directory an instance home (`assistants/home.rs`).
/// Test: `tests::the_mirrored_constants_match_the_source`.
pub const INSTRUCTIONS_FILE: &str = "instructions.md";

/// The app name whose data directory holds the ask sockets
/// (`trusty-agents-common/src/health.rs` `AGENTS_APP_NAME`).
/// Test: `tests::the_mirrored_constants_match_the_source`.
pub const AGENTS_APP_NAME: &str = "trusty-agents";

/// The `ta` binary's name (`trusty-assistants/Cargo.toml` `[[bin]] name`).
/// Test: `tests::the_daemon_hint_names_the_real_verb`.
pub const TA_BIN_NAME: &str = "ta";

/// The verb that runs one instance's ask daemon in the foreground
/// (`ask/daemon/mod.rs` `DAEMON_VERB`).
/// Test: `tests::the_daemon_hint_names_the_real_verb`.
pub const DAEMON_VERB: &str = "ask-daemon";

/// The 503 hint when no daemon serves `id`'s socket.
///
/// Why: #11748 review (ruling (a)) — `ta ask` releases the daemon it starts,
/// and the daemon then exits when idle, so it never leaves one for the
/// console. A daemon run by hand keeps its terminal's stdin open, which is
/// the lease its idle exit waits for (`ask/daemon/run.rs`), so it stays.
/// Test: `tests::the_daemon_hint_names_the_real_verb`,
/// `ask_with_no_socket_is_503_with_the_hint` (`tests/assistants_ask.rs`).
pub fn daemon_hint(id: &InstanceId) -> String {
    format!(
        "run `{TA_BIN_NAME} {DAEMON_VERB} {}` in a terminal and leave it running; the console never starts an assistant",
        id.as_str()
    )
}

/// Longest accepted instance id (`assistants/instance.rs` `MAX_ID_LEN`).
const MAX_ID_LEN: usize = 64;

/// One question for an instance's ask daemon.
/// Test: `tests::the_wire_shapes_are_pinned`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskRequest {
    /// The sender's [`PROTOCOL_VERSION`].
    pub protocol: u32,
    /// The question, as typed.
    pub question: String,
}

/// The daemon's one reply on a connection.
/// Test: `tests::the_wire_shapes_are_pinned`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskReply {
    /// The daemon's [`PROTOCOL_VERSION`].
    pub protocol: u32,
    /// The daemon's process id.
    pub daemon_pid: u32,
    /// The answer or the failure.
    #[serde(flatten)]
    pub outcome: ReplyOutcome,
}

/// What one ask produced.
/// Test: `tests::the_wire_shapes_are_pinned`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ReplyOutcome {
    /// The model's answer; the turn is in the log.
    Answer { answer: String },
    /// No answer; the turn log is unchanged.
    Error {
        kind: ReplyErrorKind,
        message: String,
    },
}

/// Why a reply carries no answer.
/// Test: `tests::the_wire_shapes_are_pinned`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyErrorKind {
    /// The request named a protocol version the daemon does not speak.
    ProtocolMismatch,
    /// The request was not a valid [`AskRequest`].
    BadRequest,
    /// The ask itself failed.
    Ask,
}

/// A validated assistant instance id: one directory name.
///
/// Why: the id arrives in a URL path. It must be refused before it reaches
/// the filesystem when it could name anything but one child of the root.
/// What: the rule of `ta`'s `AssistantInstanceId::new`, minus its trim (a
/// name that changes under the trim is not a listed directory either): 1 to
/// 64 bytes of `[a-z0-9._-]`, at least one letter or digit, never `.` or `..`.
/// Test: `tests::instance_ids_follow_the_ta_rule`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct InstanceId(String);

impl InstanceId {
    /// Validate `raw`, naming the rule it broke.
    ///
    /// # Errors
    ///
    /// A message naming the broken rule.
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            return Err("an instance id may not be blank".into());
        }
        if raw.len() > MAX_ID_LEN {
            return Err(format!("an instance id is at most {MAX_ID_LEN} characters"));
        }
        if raw == "." || raw == ".." {
            return Err("`.` and `..` are not instance ids".into());
        }
        if let Some(bad) = raw.chars().find(|c| !is_allowed(*c)) {
            return Err(format!(
                "`{}` is not allowed in an instance id; use lowercase letters, digits, `-`, `_` or `.`",
                bad.escape_default()
            ));
        }
        if !raw.chars().any(|c| c.is_ascii_alphanumeric()) {
            return Err("an instance id needs a letter or digit".into());
        }
        Ok(Self(raw.to_owned()))
    }

    /// The id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Characters an instance id may contain (`assistants/instance.rs` `is_allowed`).
fn is_allowed(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.')
}

/// The root holding one home per instance, resolved as `ta` resolves it.
///
/// What: `$TA_ASSISTANTS_DIR` when set and non-empty, else
/// `$TAGENT_ASSISTANTS_DIR` the same way, else `$HOME` (or `$USERPROFILE`)
/// joined with `trusty-agents`.
///
/// # Errors
///
/// A message naming the override when no home directory is set.
/// Test: `tests::the_root_resolves_like_ta`.
pub fn assistants_root() -> Result<PathBuf, String> {
    root_from(|key| std::env::var_os(key))
}

/// [`assistants_root`] over an injected environment.
fn root_from(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Result<PathBuf, String> {
    for key in [ASSISTANTS_DIR_ENV, LEGACY_ASSISTANTS_DIR_ENV] {
        if let Some(dir) = var(key).filter(|d| !d.is_empty()) {
            return Ok(PathBuf::from(dir));
        }
    }
    var("HOME")
        .or_else(|| var("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(ASSISTANTS_DIR_NAME))
        .ok_or_else(|| {
            format!("no home directory is set; set {ASSISTANTS_DIR_ENV} to the assistants root")
        })
}

/// The instance homes under `root`, sorted — exactly what `ta list` prints.
///
/// What: the children of `root` that are directories, carry a valid
/// [`InstanceId`] name and hold a regular [`INSTRUCTIONS_FILE`]. A missing
/// `root` is an empty list; any other read failure is returned.
/// Mirrors `crates/trusty-assistants/src/assistants/listing.rs`.
///
/// # Errors
///
/// An I/O failure reading `root`, other than its absence.
/// Test: `list_names_only_instance_homes` (`tests/assistants_routes.rs`).
pub fn list_instances(root: &Path) -> std::io::Result<Vec<InstanceId>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut found = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if !path.is_dir() || !path.join(INSTRUCTIONS_FILE).is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Ok(id) = InstanceId::parse(name) {
            found.push(id);
        }
    }
    found.sort();
    Ok(found)
}

/// `<data_dir>/trusty-agents-<id>.sock`, where an instance's ask daemon listens.
///
/// Why: #11748 dials it; landing the spelling now lets the contract test pin it.
/// What: mirrors `instance_socket_in` in `ask/daemon/mod.rs`; `data_dir` is
/// `trusty_common::resolve_data_dir(AGENTS_APP_NAME)` in production.
/// Test: `tests::the_socket_path_follows_the_ta_spelling`.
pub fn socket_path_in(data_dir: &Path, id: &InstanceId) -> PathBuf {
    data_dir.join(format!("trusty-agents-{}.sock", id.as_str()))
}
