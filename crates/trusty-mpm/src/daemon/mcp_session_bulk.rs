//! Record-only bulk deletion for the console's Sessions tab (#6431).
//!
//! Why: the console lists two registries side by side — the managed
//! `SessionManager` store and the daemon's legacy in-memory `DaemonState`
//! registry — and a legacy entry carries no `state` field at all, so the UI
//! buckets every one of them under "unknown". Clearing that bucket needs one
//! call that can delete a RECORD from whichever registry owns it. Every
//! pre-existing bulk path is the wrong tool: `session_decommission` removes the
//! workspace directory, `session_prune` operates by state over the whole fleet,
//! and the legacy `rpc::sessions_legacy_ops::remove_session` kills the tmux host
//! and decommissions the matching managed record. #1511 — a prune that
//! `rm -rf`'d a live workspace — is why this path deletes records and nothing
//! else.
//! What: [`session_delete_records`] takes an explicit id list (never a filter —
//! the caller confirms exactly what it sends), deduplicates it, and routes each
//! id to [`crate::session_manager::SessionManager::delete_record`] (managed) or
//! [`DaemonState::remove_session`] (legacy). Neither removes a worktree,
//! workspace, or any other directory. BOTH branches refuse a session that is
//! still running: the managed one through `delete_record`'s own tmux probe, the
//! legacy one through [`legacy_is_running`]. Reporting is fail-closed: a per-id
//! result carries `deleted: false` plus the error, and a failure is never
//! counted as a deletion.
//!
//! # The #1454 phantom, and why this path does not reconcile
//!
//! `rpc::sessions_legacy_ops::remove_session` decommissions any managed record
//! sharing the deleted entry's tmux name, because that path KILLS the tmux host
//! and so creates the dead-host condition a managed record would be left
//! pointing at. This path never kills a host and refuses a live one, so it only
//! ever deletes a record whose host was already gone — the phantom is
//! PRE-EXISTING here, not created.
//!
//! It is also self-healing and time-bounded: `daemon::mod`'s `reap_loop` runs
//! `reap_dead_managed_sessions` (#1744) every `REAP_INTERVAL_SECS` (60), which
//! marks a managed record whose tmux host has gone `Stopped` and leaves its
//! workspace intact. So the window is ≤60s of a stale label on a record the
//! daemon will correct on its own — for as long as that loop is running; it
//! skips a gated host state (#6348), which is the one case a twin's label stays
//! stale longer.
//!
//! `SessionManager::decommission_record_only` exists and WOULD be record-safe,
//! but is not used here: reporting suffices given that reap loop, and acting
//! would delete a record the operator never named. Plain `decommission` is worse
//! still — it removes the workspace (`remove_dir_all` under `workspace_owned`),
//! which is the #1511 behaviour this tool exists to never perform. A twin is
//! therefore REPORTED (`managed_sibling` on the result row) and left alone. See
//! [`managed_sibling_of`].
//! Test: `bulk_delete_leaves_the_workspace_on_disk`,
//! `bulk_delete_reports_partial_failure`, `bulk_delete_refuses_a_live_legacy_record`,
//! `bulk_delete_deletes_a_dead_legacy_record`,
//! `bulk_delete_refuses_every_legacy_row_when_tmux_enumeration_fails`,
//! `bulk_delete_reports_a_managed_sibling_without_touching_it`,
//! `bulk_delete_deduplicates_repeated_ids` in the `tests` module.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::core::session::SessionId;
use crate::daemon::managed_routes::record_to_json;
use crate::daemon::state::DaemonState;

/// Delete a caller-supplied set of session RECORDS, and nothing else (#6431).
///
/// Why: the Sessions tab's bulk action on the unknown bucket. The caller has
/// already shown the operator exactly which sessions it is about to delete, so
/// this takes ids rather than a server-side predicate — a filter evaluated here
/// could select a session the operator never saw in the confirmation.
/// What: for each distinct id, in the order given: parse it as a UUID; look it
/// up in the managed store and soft-delete the record via `delete_record` when
/// found (`force = false`, so a RUNNING managed session is REFUSED and reported
/// failed); otherwise drop the legacy `DaemonState` registry entry. A
/// malformed id, an id in neither registry, and a refused running session are
/// each reported as one failed result. NEVER removes a worktree, a workspace
/// directory, or any other filesystem state, and never kills a tmux host.
/// Returns `{ requested, deleted, failed, results: [{ session_id, kind,
/// deleted, error }] }`, where `requested` counts the DISTINCT ids and `kind`
/// is `"managed"`, `"legacy"`, or `null` when the id resolved to neither.
/// Test: the `tests` module below; dispatch coverage in
/// `crate::mcp::tests::dispatch_session_delete_records_tool`.
pub async fn session_delete_records(
    state: &Arc<DaemonState>,
    session_ids: &[String],
) -> Result<Value, String> {
    if session_ids.is_empty() {
        return Err("`session_ids` must name at least one session".to_string());
    }
    // #6431: ONE tmux enumeration for the whole call, so a 50-row delete does
    // not pay 50 `tmux list-sessions` round trips. Fail-closed like
    // `reap_dead_sessions`: a failed enumeration is "cannot determine", never
    // "nothing is running", and every legacy row is then refused rather than
    // deleted on an unproven assumption.
    let mgr = state.session_manager().await;
    let live_tmux: Result<HashSet<String>, String> = mgr
        .tmux_driver()
        .list_sessions()
        .map(|names| names.into_iter().collect())
        .map_err(|e| {
            format!("tmux enumeration failed ({e}) — cannot prove this session is stopped")
        });

    let mut seen: HashSet<&str> = HashSet::new();
    let mut results: Vec<Value> = Vec::new();
    let mut deleted = 0usize;
    let mut failed = 0usize;

    for raw in session_ids {
        if !seen.insert(raw.as_str()) {
            continue;
        }
        let outcome = delete_one(state, raw, live_tmux.as_ref()).await;
        match &outcome {
            Outcome::Deleted { .. } => deleted += 1,
            Outcome::Failed { .. } => failed += 1,
        }
        results.push(outcome.to_json(raw));
    }

    Ok(json!({
        "requested": results.len(),
        "deleted": deleted,
        "failed": failed,
        "results": results,
    }))
}

/// One id's outcome, kept separate from its JSON shape so the counters above
/// can never disagree with what a row reports.
enum Outcome {
    Deleted {
        kind: &'static str,
        record: Value,
    },
    Failed {
        kind: Option<&'static str>,
        error: String,
    },
}

impl Outcome {
    fn to_json(&self, session_id: &str) -> Value {
        match self {
            Self::Deleted { kind, record } => json!({
                "session_id": session_id,
                "kind": kind,
                "deleted": true,
                "error": Value::Null,
                "record": record,
            }),
            Self::Failed { kind, error } => json!({
                "session_id": session_id,
                "kind": kind,
                "deleted": false,
                "error": error,
            }),
        }
    }
}

/// Resolve one id against both registries and delete its record.
///
/// Why: the two registries answer to different types (`ManagedSessionId` vs
/// `SessionId`) and different deletion primitives, but the caller holds only a
/// UUID string — so the dispatch happens here, once, rather than in the UI.
/// What: managed store first (it is the durable one, and a managed record is
/// what an operator means by "session"), legacy registry on a miss. A managed
/// hit that `delete_record` refuses is reported failed rather than falling
/// through to the legacy branch — the id IS managed, and a fall-through would
/// report a misleading "no such record".
/// Test: the `tests` module below.
async fn delete_one(
    state: &Arc<DaemonState>,
    raw: &str,
    live_tmux: Result<&HashSet<String>, &String>,
) -> Outcome {
    let Ok(uuid) = raw.parse::<uuid::Uuid>() else {
        return Outcome::Failed {
            kind: None,
            error: format!("`{raw}` is not a valid session id (expected a UUID)"),
        };
    };

    let managed_id = crate::session_manager::ManagedSessionId::from(uuid);
    let mgr = state.session_manager().await;
    if mgr.get(&managed_id).await.is_ok() {
        // #6431: `force = false` — a RUNNING managed session is refused and
        // reported failed, never force-deleted by a bulk action.
        return match mgr.delete_record(&managed_id, false).await {
            Ok(record) => Outcome::Deleted {
                kind: "managed",
                record: record_to_json(&record),
            },
            Err(e) => Outcome::Failed {
                kind: Some("managed"),
                error: e.to_string(),
            },
        };
    }

    // #6431: legacy registry entries are the records that render as "unknown"
    // (they carry `status`, not `state`).
    let Some(session) = state.session(SessionId(uuid)) else {
        return Outcome::Failed {
            kind: None,
            error: format!("no session record found for `{raw}` in either registry"),
        };
    };

    // #6431: the legacy branch's liveness guard, the counterpart to the managed
    // branch's `delete_record` probe above. Deleting a LIVE legacy session's
    // record also unregisters its PID file, which is the one thing that lets
    // `pid_registry::sweep_orphans` find the process later — so a missing guard
    // here does not merely lose bookkeeping, it strands a running process
    // permanently.
    match legacy_is_running(&session, live_tmux) {
        Err(reason) => {
            return Outcome::Failed {
                kind: Some("legacy"),
                error: reason,
            };
        }
        Ok(true) => {
            return Outcome::Failed {
                kind: Some("legacy"),
                error: format!(
                    "session `{}` is still running — stop it first, then delete its record",
                    session.tmux_name
                ),
            };
        }
        Ok(false) => {}
    }

    // #1454: a tmux name can be tracked by BOTH registries. Report the managed
    // sibling rather than acting on it — see this module's header for why this
    // path cannot create the phantom that `rpc::sessions_legacy_ops` reconciles.
    let managed_sibling = managed_sibling_of(state, &session.tmux_name).await;

    // `DaemonState::remove_session` drops the in-memory entry, its memory
    // snapshot, and that session's own PID-file bookkeeping — it does NOT kill
    // tmux and does NOT touch a workspace. The PID-file unregister is deliberate
    // and NOT a workspace write: leaving the file behind would make the orphan
    // sweep SIGTERM a live process whose record we just dropped.
    match state.remove_session(SessionId(uuid)) {
        Some(session) => Outcome::Deleted {
            kind: "legacy",
            record: json!({
                "id": session.id.0.to_string(),
                "name": session.tmux_name,
                "status": session.status.to_string(),
                "managed_sibling": managed_sibling,
            }),
        },
        // The entry vanished between the liveness probe and the removal — a
        // concurrent reap. Report it failed rather than claiming a deletion
        // this call did not perform.
        None => Outcome::Failed {
            kind: Some("legacy"),
            error: format!("session record `{raw}` disappeared before it could be deleted"),
        },
    }
}

/// Whether a LEGACY registry entry's session is still running.
///
/// Why: the managed branch gets its answer from `delete_record`'s tmux probe;
/// a legacy entry has no such path, and every legacy entry lands in the console's
/// unknown bucket regardless of its `status` field — so without this the bulk
/// action's entire target population had no liveness protection at all. The
/// persisted `status` is NOT the answer: it is written at registration and can
/// say `Active` long after the process is gone, or lag a session that just died.
/// What: a STRICTER EXTENSION of the rule `DaemonState::reap_against` applies —
/// a tmux-hosted session is live when its `tmux_name` is in the live set; a
/// NATIVE session has no tmux session at all (checking one would call every
/// Terminal.app session dead), so it is live when its tracked PID is. It is
/// stricter in two places: `reap_against` leaves native sessions alone entirely
/// rather than probing them, and it folds an unreachable answer into "reap
/// nothing" for the whole sweep, where this refuses per row. `Err` is "could not
/// determine", never "not running": a failed tmux enumeration and a native
/// session with no tracked PID both refuse the delete.
/// Test: `bulk_delete_refuses_a_live_legacy_record`,
/// `bulk_delete_deletes_a_dead_legacy_record`,
/// `bulk_delete_refuses_a_native_legacy_record_whose_process_is_alive`,
/// `bulk_delete_refuses_every_legacy_row_when_tmux_enumeration_fails`.
fn legacy_is_running(
    session: &crate::core::session::Session,
    live_tmux: Result<&HashSet<String>, &String>,
) -> Result<bool, String> {
    use crate::core::session::SessionHost;

    match session.origin {
        SessionHost::Tmux => match live_tmux {
            Ok(live) => Ok(live.contains(&session.tmux_name)),
            Err(reason) => Err(reason.clone()),
        },
        SessionHost::Native => match session.pid {
            Some(pid) => Ok(crate::core::process::is_process_alive(pid)),
            None => Err(format!(
                "native session `{}` has no tracked pid — cannot prove it is stopped",
                session.tmux_name
            )),
        },
    }
}

/// The non-terminal MANAGED record sharing `tmux_name`, if any (#1454).
///
/// Why: both registries can track one tmux name, and
/// `rpc::sessions_legacy_ops::remove_session` decommissions the managed twin
/// after its own delete. That reconciliation exists because THAT path kills the
/// tmux host, creating the dead-host condition the managed record would then
/// point at. This path never kills a host, and refuses a live one outright, so
/// it only ever deletes a record whose host was ALREADY gone — the phantom is
/// pre-existing, and the `reap_loop`'s 60-second `reap_dead_managed_sessions`
/// sweep (#1744) corrects the twin's label to `Stopped` on its own, workspace
/// intact. `decommission_record_only` would be record-safe but is not used:
/// reporting suffices given that sweep, and acting would delete a record the
/// operator never confirmed. Plain `decommission` would also remove the twin's
/// workspace (`remove_dir_all` under `workspace_owned`), which #1511 and this
/// tool's record-only contract forbid. Naming the twin in the result is the
/// honest middle: the operator learns it exists and can act deliberately.
/// What: `Some(id)` for the first non-terminal managed record with this
/// `tmux_name`; `None` when there is none.
/// Test: `bulk_delete_reports_a_managed_sibling_without_touching_it`.
async fn managed_sibling_of(state: &Arc<DaemonState>, tmux_name: &str) -> Option<String> {
    let mgr = state.session_manager().await;
    mgr.list()
        .await
        .into_iter()
        .find(|r| r.tmux_name == tmux_name && !r.state.is_terminal())
        .map(|r| r.id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::DaemonState;
    use crate::runtime::RuntimeKind;
    use crate::session_manager::{ManagedError, ManagedSessionId, ManagedSessionState};

    /// A state whose tmux driver reports every session DEAD, so `delete_record`'s
    /// liveness guard permits a seeded record.
    async fn isolated(root: &tempfile::TempDir) -> Arc<DaemonState> {
        Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await)
    }

    /// A tmux driver that TRACKS the sessions it creates, so a seeded record
    /// reads as genuinely running — the real refusal the partial-failure test
    /// needs. A driver that answered "live" for every name would instead starve
    /// the name allocator, which asks the same question about candidate names.
    struct LiveTrackingTmux {
        live: std::sync::Mutex<HashSet<String>>,
    }

    impl LiveTrackingTmux {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                live: std::sync::Mutex::new(HashSet::new()),
            })
        }
    }

    impl crate::session_manager::ManagedTmuxDriver for LiveTrackingTmux {
        fn create_session(&self, name: &str, _workdir: &str) -> Result<(), ManagedError> {
            self.live.lock().expect("live set").insert(name.to_owned());
            Ok(())
        }
        fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
            self.live.lock().expect("live set").remove(name);
            Ok(())
        }
        fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
            Ok(String::new())
        }
        fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
            Ok(self
                .live
                .lock()
                .expect("live set")
                .iter()
                .cloned()
                .collect())
        }
    }

    async fn isolated_live_tracking(root: &tempfile::TempDir) -> Arc<DaemonState> {
        Arc::new(
            DaemonState::with_root_isolated_managed_and_driver(
                root.path().to_path_buf(),
                LiveTrackingTmux::new(),
            )
            .await,
        )
    }

    /// Seed one managed record whose workspace is a REAL directory on disk.
    async fn seed_managed(
        state: &Arc<DaemonState>,
        root: &tempfile::TempDir,
        label: &str,
    ) -> (ManagedSessionId, std::path::PathBuf) {
        let id = ManagedSessionId::new();
        let ws = root.path().join(format!("{label}-{id}"));
        std::fs::create_dir_all(&ws).expect("workspace dir");
        std::fs::write(ws.join("KEEPME.txt"), b"real work").expect("workspace file");
        let mgr = state.session_manager().await;
        mgr.create_with_id(
            id,
            format!("#6431 {label}"),
            Some(ws.clone()),
            None,
            Some(ws.clone()),
            Some("https://example.com/r.git".to_string()),
            Some("main".to_string()),
            RuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("seed managed session");
        (id, ws)
    }

    /// Register one legacy entry. `origin`/`pid` decide how liveness is probed.
    fn seed_legacy(
        state: &Arc<DaemonState>,
        tmux_name: &str,
        origin: crate::core::session::SessionHost,
        pid: Option<u32>,
    ) -> SessionId {
        use crate::core::session::{ControlModel, Session, SessionStatus};

        let id = SessionId::new();
        let mut session = Session::new(id, "/tmp/legacy", ControlModel::Tmux, None);
        // `status` is deliberately Active on every seed: the guard must key off
        // a real probe, never this field, which is what the pre-fix code did.
        session.status = SessionStatus::Active;
        session.tmux_name = tmux_name.to_string();
        session.origin = origin;
        session.pid = pid;
        state.register_session(session);
        id
    }

    /// The #1511 constraint, asserted directly: the bulk delete drops the
    /// RECORD and leaves the workspace directory (and its contents) on disk.
    ///
    /// Why: #1511 was a prune that `remove_dir_all`'d a live workspace. A bulk
    /// action driven from a UI checkbox is exactly the shape that repeats it,
    /// so the proof is a real directory with a real file in it, checked after
    /// the call.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn bulk_delete_leaves_the_workspace_on_disk() {
        let root = tempfile::TempDir::new().expect("tempdir");
        let state = isolated(&root).await;
        let (id, ws) = seed_managed(&state, &root, "record-only").await;

        let out = session_delete_records(&state, &[id.to_string()])
            .await
            .expect("bulk delete");
        assert_eq!(out["deleted"], json!(1), "{out}");
        assert_eq!(out["failed"], json!(0), "{out}");
        assert_eq!(out["results"][0]["kind"], json!("managed"), "{out}");

        assert!(ws.exists(), "the workspace directory must survive: {ws:?}");
        assert!(
            ws.join("KEEPME.txt").exists(),
            "workspace contents must survive: {ws:?}"
        );
        let mgr = state.session_manager().await;
        let after = mgr.get(&id).await.expect("record stays as a tombstone");
        assert_eq!(after.state, ManagedSessionState::Deleted);
    }

    /// A failed deletion is reported failed and never counted as a deletion.
    ///
    /// Why: the fail-closed reporting requirement — a partial run must read as
    /// partial. The refusal here is the real one: `delete_record` refuses a
    /// RUNNING managed session without `force`, and a bulk action never forces.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn bulk_delete_reports_partial_failure() {
        let root = tempfile::TempDir::new().expect("tempdir");
        let state = isolated_live_tracking(&root).await;
        let (running, ws) = seed_managed(&state, &root, "running").await;
        let ghost = uuid::Uuid::new_v4().to_string();

        let out = session_delete_records(
            &state,
            &[running.to_string(), ghost.clone(), "not-a-uuid".to_string()],
        )
        .await
        .expect("bulk delete");

        assert_eq!(out["requested"], json!(3), "{out}");
        assert_eq!(out["deleted"], json!(0), "{out}");
        assert_eq!(out["failed"], json!(3), "{out}");

        let rows = out["results"].as_array().expect("results array");
        assert_eq!(rows[0]["kind"], json!("managed"), "{out}");
        assert_eq!(rows[0]["deleted"], json!(false), "{out}");
        assert!(
            rows[0]["error"]
                .as_str()
                .unwrap_or_default()
                .contains(&running.to_string()),
            "the refusal must name the session: {out}"
        );
        assert_eq!(rows[1]["kind"], Value::Null, "{out}");
        assert_eq!(rows[2]["kind"], Value::Null, "{out}");

        // Nothing was mutated: the refused record and its workspace both stand.
        let mgr = state.session_manager().await;
        assert!(mgr.get(&running).await.is_ok());
        assert!(ws.join("KEEPME.txt").exists());
    }

    /// A LIVE legacy session is refused, exactly as a live managed one is.
    ///
    /// Why: this is the round-2 CRITICAL. Every legacy entry lands in the
    /// console's unknown bucket whatever its `status` says, so the bulk action's
    /// entire target population had no liveness protection — and deleting a live
    /// one also unregisters its PID file, the single thing that lets
    /// `pid_registry::sweep_orphans` reach that process later. The seed here
    /// carries `status: Active` deliberately: the guard must key off a real tmux
    /// probe, not the persisted field.
    /// Test: this function IS the test. It fails against the round-1 code, which
    /// deleted this session unconditionally.
    #[tokio::test]
    async fn bulk_delete_refuses_a_live_legacy_record() {
        use crate::core::session::SessionHost;

        let root = tempfile::TempDir::new().expect("tempdir");
        let state = isolated_live_tracking(&root).await;
        let id = seed_legacy(&state, "tm-legacy-live", SessionHost::Tmux, None);
        // Make the tmux host genuinely live, the way a real session would.
        state
            .session_manager()
            .await
            .tmux_driver()
            .create_session("tm-legacy-live", "/tmp")
            .expect("create tmux session");

        let out = session_delete_records(&state, &[id.0.to_string()])
            .await
            .expect("bulk delete");
        assert_eq!(out["deleted"], json!(0), "{out}");
        assert_eq!(out["failed"], json!(1), "{out}");
        assert_eq!(out["results"][0]["kind"], json!("legacy"), "{out}");
        assert!(
            out["results"][0]["error"]
                .as_str()
                .unwrap_or_default()
                .contains("still running"),
            "{out}"
        );
        assert_eq!(
            state.list_sessions().len(),
            1,
            "a live legacy record must survive the refusal"
        );
    }

    /// A legacy entry whose tmux host is gone deletes cleanly.
    ///
    /// Why: the companion to the refusal above — the guard must not make every
    /// legacy record undeletable, which would leave the unknown bucket
    /// permanently unclearable and defeat the feature.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn bulk_delete_deletes_a_dead_legacy_record() {
        use crate::core::session::SessionHost;

        let root = tempfile::TempDir::new().expect("tempdir");
        let state = isolated_live_tracking(&root).await;
        // Registered, but no tmux session was ever created for this name.
        let id = seed_legacy(&state, "tm-legacy-dead", SessionHost::Tmux, None);

        let out = session_delete_records(&state, &[id.0.to_string()])
            .await
            .expect("bulk delete");
        assert_eq!(out["deleted"], json!(1), "{out}");
        assert_eq!(out["results"][0]["kind"], json!("legacy"), "{out}");
        assert!(
            state.list_sessions().is_empty(),
            "registry entry must be gone"
        );
    }

    /// A NATIVE legacy session is probed by PID, never by tmux name.
    ///
    /// Why: a native (Terminal.app) session has no tmux session at all, so a
    /// tmux-name check would call every one of them dead — the same trap
    /// `DaemonState::reap_against` documents and skips. The live fleet carries
    /// one such record, so this is not a hypothetical row.
    /// Test: this function IS the test — `std::process::id()` is alive by
    /// definition, so the refusal is real rather than mocked.
    #[tokio::test]
    async fn bulk_delete_refuses_a_native_legacy_record_whose_process_is_alive() {
        use crate::core::session::SessionHost;

        let root = tempfile::TempDir::new().expect("tempdir");
        let state = isolated_live_tracking(&root).await;
        let id = seed_legacy(
            &state,
            "session-native",
            SessionHost::Native,
            Some(std::process::id()),
        );

        let out = session_delete_records(&state, &[id.0.to_string()])
            .await
            .expect("bulk delete");
        assert_eq!(out["deleted"], json!(0), "{out}");
        assert_eq!(out["failed"], json!(1), "{out}");
        assert_eq!(state.list_sessions().len(), 1);

        // And one with no tracked pid cannot be proven stopped, so it is refused
        // too — "could not determine" is never "not running".
        let orphan = seed_legacy(&state, "session-native-nopid", SessionHost::Native, None);
        let out = session_delete_records(&state, &[orphan.0.to_string()])
            .await
            .expect("bulk delete");
        assert_eq!(out["deleted"], json!(0), "{out}");
        assert!(
            out["results"][0]["error"]
                .as_str()
                .unwrap_or_default()
                .contains("no tracked pid"),
            "{out}"
        );
    }

    /// A failed tmux enumeration refuses every legacy row rather than deleting.
    ///
    /// Why: fail-closed, matching `reap_dead_sessions` (which reaps nothing on a
    /// failed listing) and the managed side's #5859 behaviour. Folding the error
    /// into "nothing is live" would turn a transient tmux hiccup into a
    /// fleet-wide delete of live sessions.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn bulk_delete_refuses_every_legacy_row_when_tmux_enumeration_fails() {
        use crate::core::session::SessionHost;

        struct BlindTmux;
        impl crate::session_manager::ManagedTmuxDriver for BlindTmux {
            fn create_session(&self, _n: &str, _w: &str) -> Result<(), ManagedError> {
                Ok(())
            }
            fn kill_session(&self, _n: &str) -> Result<(), ManagedError> {
                Ok(())
            }
            fn send_line(&self, _n: &str, _t: &str) -> Result<(), ManagedError> {
                Ok(())
            }
            fn capture(&self, _n: &str, _l: usize) -> Result<String, ManagedError> {
                Ok(String::new())
            }
            fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
                Err(ManagedError::TmuxUnavailable(
                    "tmux server unreachable".to_string(),
                ))
            }
        }

        let root = tempfile::TempDir::new().expect("tempdir");
        let state = Arc::new(
            DaemonState::with_root_isolated_managed_and_driver(
                root.path().to_path_buf(),
                Arc::new(BlindTmux),
            )
            .await,
        );
        let id = seed_legacy(&state, "tm-legacy-unprovable", SessionHost::Tmux, None);

        let out = session_delete_records(&state, &[id.0.to_string()])
            .await
            .expect("bulk delete");
        assert_eq!(out["deleted"], json!(0), "{out}");
        assert_eq!(out["failed"], json!(1), "{out}");
        assert!(
            out["results"][0]["error"]
                .as_str()
                .unwrap_or_default()
                .contains("cannot prove"),
            "{out}"
        );
        assert_eq!(state.list_sessions().len(), 1);
    }

    /// A managed record sharing the deleted entry's tmux name is REPORTED, and
    /// neither deleted nor decommissioned (#1454).
    ///
    /// Why: `rpc::sessions_legacy_ops::remove_session` decommissions the twin
    /// because it kills the tmux host first; this path never kills one, so it
    /// cannot create that phantom. Decommissioning here would remove the twin's
    /// workspace and delete a record the operator never confirmed — so the twin
    /// is named in the result and left alone. This test pins both halves.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn bulk_delete_reports_a_managed_sibling_without_touching_it() {
        use crate::core::session::SessionHost;

        let root = tempfile::TempDir::new().expect("tempdir");
        let state = isolated(&root).await;
        let (managed_id, ws) = seed_managed(&state, &root, "twin").await;
        let managed_name = state
            .session_manager()
            .await
            .get(&managed_id)
            .await
            .expect("managed record")
            .tmux_name;
        // A legacy entry registered under the SAME tmux name as the managed one.
        let legacy_id = seed_legacy(&state, &managed_name, SessionHost::Tmux, None);

        let out = session_delete_records(&state, &[legacy_id.0.to_string()])
            .await
            .expect("bulk delete");
        assert_eq!(out["deleted"], json!(1), "{out}");
        assert_eq!(
            out["results"][0]["record"]["managed_sibling"],
            json!(managed_id.to_string()),
            "the twin must be named in the result: {out}"
        );

        // The twin itself is untouched: still in the store, still not terminal,
        // and its workspace is still on disk.
        let twin = state
            .session_manager()
            .await
            .get(&managed_id)
            .await
            .expect("managed twin must survive");
        assert!(!twin.state.is_terminal(), "twin must not be decommissioned");
        assert!(
            ws.join("KEEPME.txt").exists(),
            "twin's workspace must stand"
        );
    }

    /// A repeated id is deleted once and counted once.
    ///
    /// Why: the confirmation dialog lists what will be deleted; a duplicate id
    /// would otherwise inflate `requested`/`failed` and report a phantom
    /// "not found" for a session that was in fact deleted.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn bulk_delete_deduplicates_repeated_ids() {
        let root = tempfile::TempDir::new().expect("tempdir");
        let state = isolated(&root).await;
        let (id, _ws) = seed_managed(&state, &root, "dupe").await;

        let out = session_delete_records(&state, &[id.to_string(), id.to_string()])
            .await
            .expect("bulk delete");
        assert_eq!(out["requested"], json!(1), "{out}");
        assert_eq!(out["deleted"], json!(1), "{out}");
        assert_eq!(out["failed"], json!(0), "{out}");
    }

    /// An empty request is a caller bug, not a silent no-op.
    #[tokio::test]
    async fn bulk_delete_rejects_an_empty_id_list() {
        let root = tempfile::TempDir::new().expect("tempdir");
        let state = isolated(&root).await;
        assert!(
            session_delete_records(&state, &[])
                .await
                .unwrap_err()
                .contains("at least one session")
        );
    }
}
