//! The staged-send table on disk and the send audit sink (#10423, S1b).
//!
//! Why: #10423 acceptance 5: the staged-send table must survive a daemon
//! restart, including unknown-outcome records, and a restart must never
//! re-post one. Acceptance 2: the audit sink returns `Result` so a failed
//! write fails closed, and no line carries message text.
//! What: paths under `~/.trusty-tools/trusty-mpm/channels/`: [`orgs_path`],
//! [`table_path`], [`send_audit_path`]. [`JsonSendStore`] writes the whole
//! table owner-only (`0600`) through a temp file and a rename;
//! [`load_table`] reads it back through [`StagedSends::restored`], treats a
//! missing file as empty and every other fault as an error (no table, no
//! send). [`JsonlSendAudit`] appends one JSON [`AuditLine`] per step.
//! Test: `send_store_tests.rs`.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::channels::staged_send::{AuditLine, SendAudit, SendStore, StagedSends, WriteFailed};

/// The `channels/` directory under `home`.
fn channels_dir(home: &Path) -> PathBuf {
    trusty_common::crate_config::crate_config_dir_at(
        home,
        crate::core::trusty_tools_config::CRATE_NAME,
    )
    .join("channels")
}

/// `~/.trusty-tools/trusty-mpm/channels/orgs.toml` (#10423).
pub fn orgs_path(home: &Path) -> PathBuf {
    channels_dir(home).join("orgs.toml")
}

/// `~/.trusty-tools/trusty-mpm/channels/staged_sends.json`.
pub fn table_path(home: &Path) -> PathBuf {
    channels_dir(home).join("staged_sends.json")
}

/// `~/.trusty-tools/trusty-mpm/channels/send_audit.jsonl`.
pub fn send_audit_path(home: &Path) -> PathBuf {
    channels_dir(home).join("send_audit.jsonl")
}

/// Create `path`'s parent owner-only when it is missing.
fn ensure_parent(path: &Path) -> Result<(), WriteFailed> {
    let Some(dir) = path.parent() else {
        return Err(WriteFailed("no parent directory".into()));
    };
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| WriteFailed(format!("create {}: {}", dir.display(), e.kind())))
}

/// Read the table at `path`.
///
/// Why: #10423: a table that cannot be read must stop sends, not start an
/// empty table that could forget an unknown-outcome record.
/// What: a missing file is a fresh table; any read or parse fault is `Err`
/// naming the path and the fault kind; a parsed table is
/// [`StagedSends::restored`].
/// Test: `table_round_trips_and_restores_unknown`, `corrupt_table_refuses_to_load`.
pub fn load_table(path: &Path) -> Result<StagedSends, WriteFailed> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StagedSends::default());
        }
        Err(e) => {
            return Err(WriteFailed(format!(
                "read {}: {}",
                path.display(),
                e.kind()
            )));
        }
    };
    serde_json::from_str::<StagedSends>(&text)
        .map(StagedSends::restored)
        .map_err(|e| WriteFailed(format!("parse {}: line {}", path.display(), e.line())))
}

/// The daemon's [`SendStore`]: one JSON file, replaced atomically.
#[derive(Debug, Clone)]
pub struct JsonSendStore {
    /// The table file.
    pub path: PathBuf,
}

impl SendStore for JsonSendStore {
    fn save(&self, table: &StagedSends) -> Result<(), WriteFailed> {
        ensure_parent(&self.path)?;
        let body = serde_json::to_vec(table).map_err(|_| WriteFailed("encode".into()))?;
        let tmp = self.path.with_extension("json.tmp");
        let fault = |what: &str, e: std::io::Error| WriteFailed(format!("{what}: {}", e.kind()));
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| fault("open", e))?;
        file.write_all(&body).map_err(|e| fault("write", e))?;
        file.sync_all().map_err(|e| fault("sync", e))?;
        fs::rename(&tmp, &self.path).map_err(|e| fault("rename", e))
    }
}

/// The daemon's [`SendAudit`]: one JSON line per step, appended and synced.
#[derive(Debug, Clone)]
pub struct JsonlSendAudit {
    /// The audit file.
    pub path: PathBuf,
}

impl SendAudit for JsonlSendAudit {
    fn record(&self, line: AuditLine) -> Result<(), WriteFailed> {
        ensure_parent(&self.path)?;
        let mut body = serde_json::to_vec(&serde_json::json!({
            "ts": now_secs(),
            "id": line.id,
            "event": line.event,
            "org": line.org,
            "code": line.code,
        }))
        .map_err(|_| WriteFailed("encode".into()))?;
        body.push(b'\n');
        let fault = |what: &str, e: std::io::Error| WriteFailed(format!("{what}: {}", e.kind()));
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&self.path)
            .map_err(|e| fault("open", e))?;
        file.write_all(&body).map_err(|e| fault("write", e))?;
        file.sync_data().map_err(|e| fault("sync", e))
    }
}

/// Unix seconds now; the clock a stage and a decision read.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
