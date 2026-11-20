//! `secrets.migrate`: the vault blob migration's socket method (#10524,
//! owner ruling 2026-11-19).
//!
//! Why: the one-prompt unlock needs every trusty Keychain item in one blob
//! item. Moving them is an explicit owner step, never a side effect of a
//! read or an unlock, because it rewrites every Keychain item at once and
//! the owner should see the backup, the counts and the names first.
//! What: refuses a caller with a Claude Code ancestor (or unknown ancestry),
//! as unlock does. Opens the Keychain backend through [`State::open`], so a
//! sandboxed server refuses. Enumerates every key the index names, then runs
//! the step in `store::blob_migrate`. A step that changed the layout ends a
//! live unlock grant (`revoked`), so no grant keeps a snapshot of the old
//! layout. One audit record per call with the step, counts and key names; a
//! failure adds its fixed reason. Never a value.
//! Test: `blob_unlock_tests.rs` (a child of `session_cache_tests.rs`).

use serde_json::Value;

use super::audit::{AuditMethod, MigrationAudit};
use super::errors::ErrorKind;
use super::gate::{Recording, audited};
use super::methods::{Caller, decode, to_json};
use super::router::State;
use super::unlock::{end_for_layout_change, person_only};
use crate::api::methods::{MigrateRequest, MigrateResponse, MigrateStep};
use crate::api::{BackendId, SecretKey, SecretsError, VaultName};
use crate::store::blob_migrate::{self, StepReport};

/// Every indexed vault with its key names.
fn indexed(state: &State) -> Result<Vec<(VaultName, Vec<SecretKey>)>, SecretsError> {
    state
        .index
        .vaults()?
        .into_iter()
        .map(|vault| {
            let keys = state.index.list(&vault)?.into_iter().map(|row| row.name);
            Ok((vault, keys.collect()))
        })
        .collect()
}

/// Run `step` against the Keychain's blob store.
fn run(state: &State, request: &MigrateRequest) -> Result<StepReport, SecretsError> {
    let backend = state.open(&BackendId::keychain())?;
    let Some(store) = backend.blob_store() else {
        return Err(SecretsError::Unsupported {
            backend: BackendId::KEYCHAIN.to_string(),
            operation: "migrate",
        });
    };
    let dry = request.dry_run;
    match request.step {
        MigrateStep::Migrate => {
            blob_migrate::migrate(&store, &blob_migrate::legacy_items(&indexed(state)?), dry)
        }
        MigrateStep::Finish => {
            blob_migrate::finish(&store, &blob_migrate::legacy_items(&indexed(state)?), dry)
        }
        MigrateStep::Rollback => blob_migrate::rollback(&store, dry),
    }
}

/// `secrets.migrate`: run one migration step.
///
/// Why: see the module docs.
/// What: params are `project` (ignored), `step` and `dry_run`. The audit log
/// is opened before any Keychain call, so an unwritable log changes nothing.
/// Test: `migrate_by_agent_descendant_is_refused_and_changes_nothing`,
/// `migrate_then_unlock_reads_the_keychain_once`,
/// `migrate_records_counts_and_names_never_values`.
pub(crate) fn migrate(state: &State, caller: Caller, params: Value) -> Result<Value, ErrorKind> {
    audited(
        state,
        caller,
        AuditMethod::Migrate,
        Recording::Once,
        |gate| {
            let Value::Object(mut fields) = params else {
                return Err(ErrorKind::InvalidParams);
            };
            fields.remove(super::methods::PROJECT_FIELD);
            let request: MigrateRequest = decode(fields)?;
            person_only(state, caller, gate)?;
            gate.backend(&BackendId::keychain());
            gate.admit()?;
            let mut audit = MigrationAudit {
                step: request.step,
                dry_run: request.dry_run,
                items: 0,
                keys: Vec::new(),
                failure: None,
            };
            let report = match run(state, &request) {
                Ok(report) => report,
                Err(err) => {
                    // #10524: a fixed reason naming items; the store builds no
                    // error text from a value.
                    audit.failure = Some(err.to_string());
                    gate.migration(audit);
                    return Err(err.into());
                }
            };
            if !request.dry_run && !report.already {
                end_for_layout_change(state);
            }
            audit.items = report.items;
            audit.keys.clone_from(&report.keys);
            gate.migration(audit);
            to_json(&MigrateResponse {
                step: request.step,
                dry_run: request.dry_run,
                already: report.already,
                items: report.items,
                keys: report.keys,
                source: report.source.map(str::to_owned),
            })
        },
    )
}
