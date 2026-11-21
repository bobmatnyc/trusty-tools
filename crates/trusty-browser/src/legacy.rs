//! The operator's `revoke <workflow>` for legacy workflow routes (`enrollment.py`).
//!
//! Why: #11239 removed workflow enrollment, but a registry may still hold
//! `routes` rows and `workflows/<name>.json` credentials; `trusty-browser
//! revoke` must still retire them, and a cleanup interrupted by a crash must
//! finish on the next run.
//! What: [`remove`] retires one route (records its registration as revoked,
//! queues its credential file for removal, deletes the route and its version)
//! and returns its profile for the caller to detach; [`finish_removals`]
//! deletes each queued credential file whose sealed ticket still matches the
//! recorded hash, keeping the record when the file cannot be read.
//! Test: `operator_revoke_retires_a_legacy_route` in `tests/parity_broker.rs`.

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};

use crate::pairing::{Fault, profile_row};
use crate::proof::proofs_equal;
use crate::pyjson::Json;
use crate::vault::Vault;

/// `digest`: lowercase hex SHA-256 of the UTF-8 text.
pub fn digest(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

fn workflow_ok(w: &str) -> bool {
    let b = w.as_bytes();
    let ok = |c: &u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    !b.is_empty()
        && b.len() <= 64
        && ok(&b[0])
        && b[1..].iter().all(|c| ok(c) || *c == b'_' || *c == b'-')
}

/// Retire `workflow`'s route; `Ok(Some(profile))` when one was removed.
pub fn remove(db: &Connection, vault: &Vault, workflow: &str) -> Result<Option<String>, Fault> {
    if !workflow_ok(workflow) {
        return Err(crate::refusal::Refusal::new("InvalidEnrollment").into());
    }
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    let profile: Option<String> = tx
        .query_row(
            "SELECT profile FROM routes WHERE workflow=?",
            [workflow],
            |r| r.get(0),
        )
        .optional()?;
    let Some(profile) = profile else {
        return Ok(None);
    };
    let Some(row) = profile_row(&tx, &profile)? else {
        return Ok(None);
    };
    let key = row.key(vault)?;
    let original = Json::Array(
        [
            Some(row.profile.as_str()),
            Some(row.email.as_str()),
            row.workflow.as_deref(),
            row.url.as_deref(),
            row.account.as_deref(),
            Some(key.as_str()),
        ]
        .into_iter()
        .map(|v| v.map_or(Json::Null, Json::from))
        .collect(),
    );
    let text = original
        .dumps(false)
        .map_err(|e| Fault::Internal(e.to_string()))?;
    let ticket = row.ticket.clone().unwrap_or_default();
    tx.execute("INSERT OR IGNORE INTO revoked VALUES (?)", [digest(&text)])?;
    tx.execute(
        "INSERT OR REPLACE INTO pending_removals VALUES (?,?)",
        [workflow, ticket.as_str()],
    )?;
    tx.execute(
        "DELETE FROM pending_credentials WHERE workflow=?",
        [workflow],
    )?;
    tx.execute("DELETE FROM routes WHERE workflow=?", [workflow])?;
    tx.execute("DELETE FROM route_versions WHERE workflow=?", [workflow])?;
    tx.commit()?;
    Ok(Some(profile))
}

/// `read_credential`: the clear ticket of a 0600, owner-only, non-symlink credential file.
fn read_credential(path: &Path, vault: &Vault, workflow: &str) -> Result<String, Fault> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| Fault::Internal(e.to_string()))?;
    let meta = file
        .metadata()
        .map_err(|e| Fault::Internal(e.to_string()))?;
    if !meta.is_file() || meta.uid() != crate::private::uid() || meta.mode() & 0o777 != 0o600 {
        return Err(Fault::Internal("CredentialConflict".into()));
    }
    let mut raw = Vec::new();
    file.by_ref()
        .take(2048)
        .read_to_end(&mut raw)
        .map_err(|e| Fault::Internal(e.to_string()))?;
    let fields = Json::parse(&raw).map_err(|e| Fault::Internal(e.to_string()))?;
    let shaped = crate::pairing::key_set_is(&fields, &["workflow", "ticket"])
        && fields.get("workflow").and_then(Json::as_str) == Some(workflow);
    let sealed = fields
        .get("ticket")
        .and_then(Json::as_str)
        .filter(|_| shaped);
    let sealed = sealed.ok_or_else(|| Fault::Internal("CredentialConflict".into()))?;
    Ok(vault.unseal(&format!("ticket:{workflow}"), sealed)?)
}

/// Retry credential-file cleanup after a process death during revoke.
pub fn finish_removals(db: &Connection, vault: &Vault, state: &Path) -> Result<(), Fault> {
    let pending: Vec<(String, String)> = {
        let mut stmt = db.prepare("SELECT workflow,ticket_hash FROM pending_removals")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    for (workflow, hash) in pending {
        let path = state.join("workflows").join(format!("{workflow}.json"));
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            _ => match read_credential(&path, vault, &workflow) {
                Ok(token) => {
                    if proofs_equal(&digest(&token), &hash) {
                        let _ = fs::remove_file(&path);
                    }
                }
                // Corrupt, inaccessible or foreign credentials are never removed.
                Err(_) => continue,
            },
        }
        db.execute("DELETE FROM pending_removals WHERE workflow=?", [&workflow])?;
    }
    Ok(())
}
