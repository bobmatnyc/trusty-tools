//! One audited call: what it names, when it may change a backend, and the
//! records it leaves (#4567).
//!
//! Why: DOC-45 C-7.12 — an allowed change must never happen without its
//! audit record, and a refusal must never be blocked by the audit. The
//! router sees the method and the outcome but not the vault or key, so each
//! method body fills in its subject as it learns it, through a [`Gate`].
//! What: [`audited`] runs a body with a gate and settles the call's records.
//! The ordering for an allowed `set`, `delete` or `copy`:
//! 1. Every check runs first. A failure is a deny record, best-effort: the
//!    deny reply goes out whether or not the record was written.
//! 2. [`Gate::admit`] opens and checks the audit log and holds it open. If
//!    that fails, [`unaudited_allow`] decides; fail-closed returns
//!    [`ErrorKind::AuditUnavailable`] and the backend is never called.
//! 3. The backend call runs, then its record (allow, or deny with the
//!    failure's kind) is appended and synced through the held handle, before
//!    the reply.
//! 4. If that append fails after a successful backend call, the change stands
//!    — it cannot be undone without another unaudited change — and the reply
//!    is [`ErrorKind::AuditUnavailable`], so no success reply ever goes out
//!    without its record. `copy` also stops before its next key.
//!
//! When the machine config sets `secrets.audit: false`, the gate writes
//! nothing and opens nothing, so step 2 cannot refuse.
//! Test: `audit_tests.rs`.

use std::path::PathBuf;

use serde_json::Value;

use super::audit::{AuditFile, AuditMethod, AuditRecord};
use super::errors::ErrorKind;
use super::project::ProjectContext;
use super::router::State;
use crate::api::{BackendId, SecretKey, VaultName};
use crate::store::config::{ProjectSecretsConfig, load_machine_at};
use crate::store::platform::now_unix;

/// Whether an allowed change is refused when its audit record cannot be
/// guaranteed.
// #4567: owner ruling 2b, "Fail closed on allow" (2026-10-06). This constant
// is the only branch point; `false` would make the allow path fail-open.
const FAIL_CLOSED_ON_ALLOW: bool = true;

/// Apply [`FAIL_CLOSED_ON_ALLOW`]: `Err` refuses the allowed change.
///
/// Test: `audit_unwritable_sink_refuses_an_allowed_set_before_the_backend`,
/// `post_success_append_failure_is_audit_unavailable_and_the_change_stands`,
/// `copy_stops_after_a_key_whose_record_cannot_be_appended`,
/// `copy_stops_before_the_next_key_after_a_failed_deny_append`.
fn unaudited_allow() -> Result<(), ErrorKind> {
    if FAIL_CLOSED_ON_ALLOW {
        Err(ErrorKind::AuditUnavailable)
    } else {
        Ok(())
    }
}

/// Which records a method leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recording {
    /// One per call, allow or deny (`set`, `delete`).
    Once,
    /// One per key moved, through [`Gate::record_key`], plus one deny for a
    /// refusal before any key moves (`copy`).
    PerKey,
    /// One per denied call; none when allowed (`list`).
    DenyOnly,
}

/// The audit log, held open from admission to the last record.
struct Admitted {
    /// `None` when suppressed, or when fail-open let the call proceed.
    file: Option<AuditFile>,
    /// A record failed to append; later allowed changes must stop.
    broken: bool,
}

impl Admitted {
    fn append(&mut self, record: &AuditRecord) -> Result<(), ()> {
        if self.broken {
            return Err(());
        }
        match &mut self.file {
            None => Ok(()),
            Some(file) => file.append(record).map_err(|_| self.broken = true),
        }
    }
}

/// One audited call's state.
pub(crate) struct Gate<'a> {
    state: &'a State,
    method: AuditMethod,
    recording: Recording,
    suppressed: bool,
    vault: Option<VaultName>,
    key: Option<SecretKey>,
    backend: Option<BackendId>,
    project_root: Option<PathBuf>,
    admitted: Option<Admitted>,
}

/// Run `body` under a gate for `method`, then write its outstanding record.
///
/// What: an `Err` from `body` before [`Gate::admit`] is a best-effort deny
/// record. After admission, a [`Recording::Once`] call appends its one
/// record; an append failure turns an `Ok` into
/// [`ErrorKind::AuditUnavailable`] (fail-closed) and leaves an `Err` as it
/// was. [`Recording::PerKey`] records were written by the body.
/// Test: `audit_set_and_delete_write_one_record_per_call`,
/// `audit_unwritable_sink_still_returns_the_deny_reply`.
pub(crate) fn audited(
    state: &State,
    method: AuditMethod,
    recording: Recording,
    body: impl FnOnce(&mut Gate<'_>) -> Result<Value, ErrorKind>,
) -> Result<Value, ErrorKind> {
    let mut gate = Gate {
        state,
        method,
        recording,
        suppressed: suppressed_by_machine(state),
        vault: None,
        key: None,
        backend: None,
        project_root: None,
        admitted: None,
    };
    let result = body(&mut gate);
    gate.finish(result)
}

impl Gate<'_> {
    /// The vault and key the request names, once decoded.
    pub(crate) fn name(&mut self, vault: &VaultName, key: Option<&SecretKey>) {
        self.vault = Some(vault.clone());
        self.key = key.cloned();
    }

    /// The backend the call writes to.
    pub(crate) fn backend(&mut self, backend: &BackendId) {
        self.backend = Some(backend.clone());
    }

    /// The resolved project: its root, and its backend unless one was named.
    pub(crate) fn project(&mut self, project: &ProjectContext) {
        self.project_root = Some(project.root().to_path_buf());
        if self.backend.is_none() {
            self.backend = Some(project.resolved_config().backend);
        }
    }

    /// Open the audit log before the backend is touched.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::AuditUnavailable`] when the log cannot be opened and
    /// [`FAIL_CLOSED_ON_ALLOW`] refuses.
    pub(crate) fn admit(&mut self) -> Result<(), ErrorKind> {
        let file = if self.suppressed {
            None
        } else {
            match self.state.audit.open() {
                Ok(file) => Some(file),
                Err(_) => {
                    unaudited_allow()?;
                    None
                }
            }
        };
        self.admitted = Some(Admitted {
            file,
            broken: false,
        });
        Ok(())
    }

    /// Whether another allowed change may start: not after a failed append.
    pub(crate) fn ready(&self) -> Result<(), ErrorKind> {
        match &self.admitted {
            Some(admitted) if admitted.broken => unaudited_allow(),
            _ => Ok(()),
        }
    }

    /// Record one copied key: allowed when `outcome` is `Ok`.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::AuditUnavailable`] when an allowed key's record failed to
    /// append and [`FAIL_CLOSED_ON_ALLOW`] refuses; a denied key's record is
    /// best-effort.
    pub(crate) fn record_key(
        &mut self,
        key: &SecretKey,
        outcome: Result<(), ErrorKind>,
    ) -> Result<(), ErrorKind> {
        let mut record = self.record(outcome.err());
        record.key = Some(key.clone());
        let Some(admitted) = self.admitted.as_mut() else {
            return Err(ErrorKind::Internal);
        };
        match (admitted.append(&record), outcome) {
            (Err(()), Ok(())) => unaudited_allow(),
            _ => Ok(()),
        }
    }

    /// A record of this call's subject, allowed or denied with `denied`.
    fn record(&self, denied: Option<ErrorKind>) -> AuditRecord {
        // #4567: S8 passes the caller pid; until then it is unknown.
        let mut record = AuditRecord::new(now_unix(), self.method, denied).with_caller_pid(None);
        record.vault = self.vault.clone();
        record.key = self.key.clone();
        record.backend = self.backend.clone();
        record.project_root = self.project_root.clone();
        record
    }

    fn finish(mut self, result: Result<Value, ErrorKind>) -> Result<Value, ErrorKind> {
        let Some(mut admitted) = self.admitted.take() else {
            if let Err(kind) = &result {
                self.deny_best_effort(*kind);
            }
            return result;
        };
        if self.recording != Recording::Once {
            return result;
        }
        let record = self.record(result.as_ref().err().copied());
        match (admitted.append(&record), result) {
            (Err(()), Ok(value)) => unaudited_allow().map(|()| value),
            (_, result) => result,
        }
    }

    /// Append a deny record if the log opens; ignore every failure.
    fn deny_best_effort(&self, kind: ErrorKind) {
        if self.suppressed {
            return;
        }
        if let Ok(mut file) = self.state.audit.open() {
            let _ = file.append(&self.record(Some(kind)));
        }
    }
}

/// Whether the untracked machine config turns the audit off.
///
/// What: only `secrets.audit: false` suppresses. A machine config that is
/// missing or does not parse leaves the audit on.
/// Test: `audit_machine_suppression_lets_set_succeed_with_an_unwritable_sink`.
fn suppressed_by_machine(state: &State) -> bool {
    matches!(
        load_machine_at(&state.settings.machine_config),
        Ok(Some(machine)) if machine.audit == Some(false)
    )
}

/// Refuse a tracked project config that turns the audit off.
///
/// Why: DOC-45 C-7.10 — anyone who lands a change in the repository could
/// otherwise silence the audit. Refused, not ignored, as #9326 refuses a
/// tracked `backend: file`, so the operator sees the setting has no effect.
/// What: project `secrets.audit: false` is
/// [`ErrorKind::TrackedAuditRefused`]; `true` or absent passes.
/// Test: `audit_tracked_project_config_cannot_suppress_the_audit`.
pub(crate) fn check_tracked_audit(config: Option<&ProjectSecretsConfig>) -> Result<(), ErrorKind> {
    match config.and_then(|c| c.audit) {
        Some(false) => Err(ErrorKind::TrackedAuditRefused),
        _ => Ok(()),
    }
}
