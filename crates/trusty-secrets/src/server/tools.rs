//! Secrets tools trusty-secrets has no backend for, detected for doctor
//! (#7519 P4, DOC-74 §7, owner amendment 5).
//!
//! Why: the owner asked that the secrets manager "auto-detect any secrets
//! management tools running locally" (2026-09-11). Detection must be
//! deterministic and report-only: nothing it runs may prompt or unlock.
//! What: [`detect`] looks each tool's program up in the absolute entries of
//! the `PATH` the server read at start, through the same search the CLI
//! backends use (`store::program`). It runs nothing, reads no config and
//! makes no network call. Every tool here is unsupported: the row says
//! whether it is installed and where, so `tm secrets doctor` can name it.
//! Test: `doctor_detects_unsupported_tools_on_the_start_path_without_running_them`,
//! `binary_doctor_detects_tools_on_its_start_path`.

use std::ffi::OsStr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::store::program::find_on_path;

/// DOC-74 §7's detected-but-unsupported tools: id, then program name.
const UNSUPPORTED: [(&str, &str); 6] = [
    ("bitwarden", "bw"),
    ("vault", "vault"),
    ("pass", "pass"),
    ("gopass", "gopass"),
    ("doppler", "doppler"),
    ("infisical", "infisical"),
];

/// One detected tool in [`super::DoctorResponse::tools`].
///
/// What: `supported` is false for every tool listed today: trusty-secrets
/// has no backend for it. `path` is the program found, when `installed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DetectedTool {
    /// The tool's id, e.g. `bitwarden`.
    pub id: String,
    /// The program looked up, e.g. `bw`.
    pub program: String,
    /// Whether an executable `program` is in an absolute `PATH` entry.
    pub installed: bool,
    /// Where it was found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Whether trusty-secrets has a backend for it.
    #[serde(default)]
    pub supported: bool,
}

/// Every unsupported tool, installed or not, in a fixed order.
///
/// What: a `None` or empty `search_path` finds nothing; relative and empty
/// entries are skipped.
pub(crate) fn detect(search_path: Option<&OsStr>) -> Vec<DetectedTool> {
    UNSUPPORTED
        .iter()
        .map(|&(id, program)| {
            let path = find_on_path(program, search_path);
            DetectedTool {
                id: id.to_string(),
                program: program.to_string(),
                installed: path.is_some(),
                path,
                supported: false,
            }
        })
        .collect()
}
