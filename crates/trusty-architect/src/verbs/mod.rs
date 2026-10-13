//! The `tm architect <verb>` commands: the Rust stand-ins for the prototype's
//! scripts (#9822, #9171 A4).
//!
//! Why: `start-fleet-poll.sh` and the Architect's skills call five Python
//! scripts. Each verb here takes the same arguments and prints the same
//! stdout as the script it replaces, so a caller can swap one for the other;
//! `tm` only parses arguments and prints. Where the prototype prints a
//! default and exits 0 on a failure, the verb fails closed instead.
//! What: [`classify`] (a capture's `parse_pane` facts as `json.dumps`),
//! [`input_state`] (`input-state.py`), [`quiet`] (the quiet-session list),
//! [`self_ctx`] (the Architect's own context tokens) and [`poll`]
//! (`fleet-poll.py`). Each returns stdout text or a [`VerbError`] whose
//! [`VerbError::exit_code`] is 2 for a bad argument (argparse's code) and 1
//! for input that cannot be read, a tmux that cannot be reached, or state
//! that cannot be parsed.
//! Test: `tests/verbs_parity.rs` (stdout against the Python's recorded
//! output), `tests/verbs_poll.rs`, and trusty-mpm's `tests/tm_architect_verbs.rs`
//! (the `tm` binary).

mod input_state;
mod poll;

use std::path::{Path, PathBuf};

pub use input_state::{InputTarget, input_state};
pub use poll::{PollArgs, poll, poll_config};

use crate::classify::parse_pane;
use crate::pyjson::to_python_json;
use crate::quiet::read_quiet_sessions_checked;
use crate::self_ctx::self_ctx_tokens;

/// Why a verb did not print a result.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VerbError {
    /// A bad argument or setting; exit 2, as argparse exits on one.
    #[error("{0}")]
    Usage(String),
    /// The input could not be read, tmux could not be reached, or state could
    /// not be parsed; exit 1. Never reported as an empty result.
    #[error("{0}")]
    Failed(String),
}

impl VerbError {
    /// The process exit status for this error.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => 2,
            Self::Failed(_) => 1,
        }
    }
}

/// What a verb printed, and whether it then failed.
///
/// Why: `input-state.py` prints one line per target, a `missing` line
/// included, and keeps going; the verb prints the same lines and then fails.
#[derive(Debug)]
pub struct Output {
    /// The verb's stdout, byte for byte the script's.
    pub stdout: String,
    /// Set when the verb must exit non-zero after printing `stdout`.
    pub error: Option<VerbError>,
}

/// The Architect project directory: `ARCHITECT_PROJECT_DIR` when set and not
/// empty, else `cwd`. The prototype's scripts use the directory above their
/// own `scripts/`; `start-fleet-poll.sh` starts the poller there.
pub fn project_root(get: &dyn Fn(&str) -> Option<String>, cwd: &Path) -> PathBuf {
    get("ARCHITECT_PROJECT_DIR")
        .filter(|v| !v.is_empty())
        .map_or_else(|| cwd.to_path_buf(), PathBuf::from)
}

/// `json.dumps(parse_pane(capture))` plus a newline.
///
/// # Errors
/// [`VerbError::Failed`] for an empty capture (tmux always prints rows) and
/// for bytes that are not UTF-8.
pub fn classify(capture: &[u8]) -> Result<String, VerbError> {
    if capture.is_empty() {
        return Err(VerbError::Failed(
            "empty capture: nothing to classify".to_owned(),
        ));
    }
    let text = std::str::from_utf8(capture)
        .map_err(|e| VerbError::Failed(format!("capture is not UTF-8: {e}")))?;
    let json = to_python_json(&parse_pane(text), true)
        .map_err(|e| VerbError::Failed(format!("cannot render the pane facts: {e}")))?;
    Ok(format!("{json}\n"))
}

/// The quiet-session names in `path`, sorted, one per line.
///
/// # Errors
/// [`VerbError::Failed`] when the file cannot be read, a missing file
/// included: the poller reads that as an empty list, but this verb's answer
/// would then be a default, not the list.
pub fn quiet(path: &Path) -> Result<String, VerbError> {
    let names = read_quiet_sessions_checked(path).map_err(|e| {
        VerbError::Failed(format!(
            "cannot read the quiet-sessions file {}: {e}",
            path.display()
        ))
    })?;
    Ok(names.iter().map(|n| format!("{n}\n")).collect())
}

/// `print(self_ctx_tokens(directory))`: the Architect's own context tokens.
///
/// # Errors
/// [`VerbError::Failed`] when there is nothing to measure (no directory, no
/// interactive transcript, no assistant usage; the prototype prints `None`)
/// and when the transcript cannot be read or its usage is not a number.
pub fn self_ctx(directory: &Path) -> Result<String, VerbError> {
    match self_ctx_tokens(directory) {
        Ok(Some(tokens)) => Ok(format!("{tokens}\n")),
        Ok(None) => Err(VerbError::Failed(format!(
            "no interactive transcript with assistant usage in {}",
            directory.display()
        ))),
        Err(e) => Err(VerbError::Failed(e.to_string())),
    }
}
