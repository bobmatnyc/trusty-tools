//! The broker: grants, the desktop lease and the operator ops around one supervised daemon.
//!
//! Why: agents reach the desktop only through this component, which owns the
//! supervisor and is therefore the only caller of the private daemon (plan
//! section 4; port of the spike's `broker.py`).
//! What: [`Broker`] holds one [`Inner`] under a mutex: the [`Supervisor`],
//! the [`Audit`] log, the [`Grants`] table, the [`DesktopLease`], a
//! [`Journal`] per grant and the last redacted snapshot per window.
//! [`Broker::handle`] answers one request line for a session: `connect`
//! (redeems a `tc-` code), `release`, `disconnect`, `grants`, `wait`,
//! `refresh`, `status` and `call` on the agent socket; `status`, `start`,
//! `stop`, `restart`, `grant_request`, `grant_confirm`, `grants`, `lease`,
//! `revoke` and `lockout` on the operator socket (`broker_operator.rs`). Every refusal is audited, and
//! an audit failure refuses. Whenever the lease moves, the broker rescopes
//! the supervisor to the new holder's grant ([`Inner::moved`]). The call
//! pipeline is in `broker_call.rs`.
//! Test: `tests/broker_spike.rs`, `tests/broker_lease.rs`, `tests/broker_fail_closed.rs`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::app_identity::{AppResolver, LsappinfoResolver};
use crate::audit::Audit;
use crate::broker_operator::ConsentState;
use crate::consent::{ConsentDialog, DIALOG_TIMEOUT};
use crate::grants::{Grant, GrantError, Grants, OperatorMint};
use crate::journal::Journal;
use crate::launcher::Launcher;
use crate::lease::{Claim, Connect, DesktopLease};
use crate::manifest::LeaseScope;
use crate::supervisor::Supervisor;

/// Seconds, injectable so tests drive the lease and grant clocks.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// Unix time in seconds.
pub fn wall_clock() -> Clock {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64())
    })
}

/// Who sent a request.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub session: String,
    pub uid: u32,
    /// True on `operator.sock`.
    pub operator: bool,
}

/// A refusal reply.
pub fn refusal(code: &str, detail: &str) -> Value {
    json!({"ok": false, "code": code, "error": detail})
}

/// The broker's state; one lock serialises every request.
pub struct Inner<L: Launcher> {
    pub supervisor: Supervisor<L>,
    pub(crate) audit: Audit,
    pub(crate) grants: Grants,
    pub(crate) lease: DesktopLease,
    pub(crate) journals: HashMap<String, Journal>,
    /// (pid, window_id) -> (tokens, fingerprint) of the last redacted snapshot.
    pub(crate) snaps: HashMap<(u32, u64), (BTreeSet<String>, String)>,
    /// False after a failed rescope or restart; a call then rescopes first.
    pub(crate) scope_ready: bool,
    pub(crate) consent: ConsentState,
    /// `<state>/lockout`: present while a lockout holds, across restarts.
    pub(crate) lockout_marker: PathBuf,
    /// Names the app behind a pid; re-checked at confirm and at every call (#11460).
    pub(crate) resolver: Arc<dyn AppResolver>,
}

/// The broker.
pub struct Broker<L: Launcher> {
    inner: Mutex<Inner<L>>,
    clock: Clock,
    sessions: AtomicU64,
    dialog: Arc<dyn ConsentDialog>,
    dialog_timeout: std::time::Duration,
    pub(crate) shutdown: AtomicBool,
}

impl<L: Launcher> Broker<L> {
    /// A broker over `supervisor` whose audit log and grants live in `state`.
    ///
    /// `dialog` shows the consent prompt; only the broker bin passes the real one.
    /// A malformed or shared `grants.json` refuses to start (fail closed).
    /// A `<state>/lockout` marker starts the broker locked out (#11460):
    /// every op but `status` and the operator unlock is refused.
    pub fn new(
        supervisor: Supervisor<L>,
        state: &Path,
        clock: Clock,
        dialog: Arc<dyn ConsentDialog>,
    ) -> Result<Self, GrantError> {
        let grants = Grants::load(&state.join("grants.json"), clock())?;
        let lockout_marker = state.join(crate::broker_unlock::LOCKOUT_MARKER);
        let mut consent = ConsentState::default();
        // #11460: an unreadable marker path is treated as present.
        consent.locked_down = !matches!(lockout_marker.try_exists(), Ok(false));
        Ok(Broker {
            inner: Mutex::new(Inner {
                supervisor,
                audit: Audit::new(state.join("audit.jsonl")),
                grants,
                lease: DesktopLease::new(),
                journals: HashMap::new(),
                snaps: HashMap::new(),
                scope_ready: false,
                consent,
                lockout_marker,
                resolver: Arc::new(LsappinfoResolver),
            }),
            clock,
            sessions: AtomicU64::new(0),
            dialog,
            dialog_timeout: DIALOG_TIMEOUT,
            shutdown: AtomicBool::new(false),
        })
    }

    /// The same broker with a shorter dialog timeout.
    pub fn with_dialog_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.dialog_timeout = timeout;
        self
    }

    /// The same broker resolving pids with `resolver` (tests pass a fake).
    pub fn with_app_resolver(mut self, resolver: Arc<dyn AppResolver>) -> Self {
        self.inner
            .get_mut()
            .unwrap_or_else(|p| p.into_inner())
            .resolver = resolver;
        self
    }

    pub(crate) fn resolver(&self) -> Arc<dyn AppResolver> {
        Arc::clone(&self.lock().resolver)
    }

    pub(crate) fn dialog(&self) -> Arc<dyn ConsentDialog> {
        Arc::clone(&self.dialog)
    }

    pub(crate) fn dialog_timeout(&self) -> std::time::Duration {
        self.dialog_timeout
    }

    /// The state, locked. A poisoned lock is recovered: every field stays consistent per request.
    pub fn lock(&self) -> MutexGuard<'_, Inner<L>> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn now(&self) -> f64 {
        (self.clock)()
    }

    /// A new session id, one per connection.
    pub fn open_session(&self) -> String {
        format!("s-{}", self.sessions.fetch_add(1, Ordering::SeqCst) + 1)
    }

    /// The session's connection closed: release its lease or queue place.
    pub fn close_session(&self, ctx: &Ctx) {
        let now = self.now();
        let mut inner = self.lock();
        inner.consent.unbind(&ctx.session);
        if inner.lease.position(&ctx.session).is_some() {
            let moved = inner.lease.disconnect(&ctx.session, now);
            let _ = inner.audited(base(ctx, "disconnect"), json!({"ok": true}));
            inner.moved(moved, ctx);
        }
    }

    /// Operator path, in process: mint a grant from a consented request.
    ///
    /// Test: `mint_audit_failure_revokes_the_grant`, `mint_refuses_sensitive_apps_and_denied_tools`.
    pub fn operator_mint(&self, req: OperatorMint) -> Result<Grant, Value> {
        let now = self.now();
        self.lock().mint(req, now)
    }

    /// Operator path, in process: revoke a grant and drop its lease claims.
    pub fn operator_revoke(&self, id: &str) -> Result<(), Value> {
        let now = self.now();
        let ctx = Ctx {
            session: "operator".into(),
            uid: crate::private::uid(),
            operator: true,
        };
        let reply = self.lock().revoke(id, base(&ctx, "revoke"), &ctx, now);
        if reply["ok"] == json!(true) {
            Ok(())
        } else {
            Err(reply)
        }
    }

    /// Answer one request line (`None` when it was not JSON).
    ///
    /// Why: one entry point, so every op passes the lease clock and the audit.
    /// What: expires an idle holder or a dead holder grant first (rescoping
    /// to the promoted one), then dispatches on `op`.
    /// Test: `broker_bad_json_is_refused`, `every_gate_refusal_never_reaches_driver`.
    pub fn handle(&self, request: Option<&Value>, ctx: &Ctx) -> Value {
        let started = std::time::Instant::now();
        // Ops that must not hold the broker lock while they wait.
        let op = request
            .and_then(Value::as_object)
            .and_then(|r| r.get("op"))
            .and_then(Value::as_str);
        if ctx.operator && op == Some("unlock_request") {
            return self.unlock_request(ctx);
        }
        if let Some(req) = request.and_then(Value::as_object)
            && !self.lock().consent.locked_down
        {
            match (ctx.operator, req.get("op").and_then(Value::as_str)) {
                (true, Some("grant_request")) => return self.grant_request(req, ctx),
                (true, Some("lockout")) => return self.lockout(ctx),
                (false, Some("wait")) => return self.wait(req, ctx),
                _ => {}
            }
        }
        let now = self.now();
        let mut inner = self.lock();
        inner.tick(now, ctx);
        let Some(req) = request.and_then(Value::as_object) else {
            return inner.refuse(
                base(ctx, "none"),
                "bad_request",
                "request must be a JSON object",
            );
        };
        let op = req.get("op").and_then(Value::as_str).unwrap_or("none");
        let mut rec = base(ctx, op);
        if let Some(rid) = req.get("request_id") {
            rec.insert("request_id".into(), rid.clone());
        }
        let unlocking = ctx.operator && op == "unlock_confirm";
        if inner.consent.locked_down && op != "status" && !unlocking {
            return inner.refuse(rec, "locked_out", "the operator locked the broker out");
        }
        let id = req.get("grant_id").and_then(Value::as_str).unwrap_or("");
        match (ctx.operator, op) {
            (_, "status") => inner.status(rec, ctx),
            (true, "grant_confirm") => inner.grant_confirm(req, rec, now),
            (true, "unlock_confirm") => inner.unlock_confirm(req, rec, now),
            (true, "grants") => inner.list_grants(rec, now),
            (true, "lease") => inner.lease_view(rec),
            (true, "revoke") => inner.revoke(id, rec, ctx, now),
            (false, "grants") => inner.session_grants(rec, ctx, now),
            (false, "disconnect") => {
                let moved = inner.lease.release(&ctx.session, now);
                inner.consent.unbind(&ctx.session);
                let reply = inner.audited(rec, json!({"ok": true}));
                inner.moved(moved, ctx);
                reply
            }
            (false, "refresh") => inner.refresh(id, rec, ctx, now),
            (false, "connect") => inner.connect(req, rec, ctx, now),
            (false, "release") => {
                let moved = inner.lease.release(&ctx.session, now);
                let reply = inner.audited(rec, json!({"ok": true}));
                inner.moved(moved, ctx);
                reply
            }
            (false, "call") => inner.call(req, rec, ctx, now, started),
            (true, "start" | "stop" | "restart") => inner.operator(op, rec),
            _ => inner.refuse(rec, "unknown_op", op),
        }
    }
}

/// The audit record every line starts from.
pub(crate) fn base(ctx: &Ctx, op: &str) -> Map<String, Value> {
    let mut rec = Map::new();
    rec.insert("op".into(), json!(op));
    rec.insert("uid".into(), json!(ctx.uid));
    rec.insert("session".into(), json!(ctx.session));
    rec
}

impl<L: Launcher> Inner<L> {
    /// The grant `id`, alive or not.
    pub fn grant(&self, id: &str) -> Option<&Grant> {
        self.grants.get(id)
    }

    /// The `request_id` journal of `grant`, once it has one.
    pub fn journal(&self, grant: &str) -> Option<&Journal> {
        self.journals.get(grant)
    }

    /// Write `record`; on failure answer `audit_failed` instead of `reply`.
    pub(crate) fn audited(&mut self, record: Map<String, Value>, reply: Value) -> Value {
        match self.audit.write(&record) {
            Ok(()) => reply,
            Err(e) => refusal("audit_failed", &e.to_string()),
        }
    }

    /// Audit a refusal and answer it.
    pub(crate) fn refuse(
        &mut self,
        mut record: Map<String, Value>,
        code: &str,
        detail: &str,
    ) -> Value {
        record.insert("refused".into(), json!(code));
        self.audited(record, refusal(code, detail))
    }

    fn tick(&mut self, now: f64, ctx: &Ctx) {
        if let Some((gone, moved)) = self.lease.expire(now) {
            let mut rec = base(ctx, "lease_expired");
            rec.insert("expired_session".into(), json!(gone));
            let _ = self.audit.write(&rec);
            self.moved(moved, ctx);
        }
        let dead = self
            .lease
            .holder()
            .map(|h| h.grant.clone())
            .filter(|g| self.grants.live(g, now).is_err());
        if let Some(grant) = dead {
            self.journals.remove(&grant);
            let moved = self.lease.drop_grant(&grant, now);
            self.moved(moved, ctx);
        }
    }

    /// The lease moved to `holder`: forget snapshots, rescope to its grant.
    ///
    /// Why: a token from another window or daemon instance must never be accepted,
    /// and the bounded daemon admits only the holder's pid and window.
    /// What: clears snapshots; for a new holder, rescopes the supervisor
    /// (a restart only when the scope differs). A failure leaves
    /// `scope_ready` false, so the holder's next call retries and is refused
    /// on a second failure.
    /// Test: `handoff_rescopes_to_the_next_grant`, `failed_rescope_refuses_the_call`.
    pub(crate) fn moved(&mut self, holder: Option<Claim>, ctx: &Ctx) {
        let Some(claim) = holder else { return };
        self.snaps.clear();
        let mut rec = base(ctx, "lease_moved");
        rec.insert("holder".into(), json!(claim.session));
        rec.insert("grant".into(), json!(claim.grant));
        let scope = self.grants.get(&claim.grant).map(|g| g.scope);
        let outcome = match scope {
            Some(scope) => self.rescope(scope),
            None => Err("grant vanished".to_owned()),
        };
        if let Err(e) = &outcome {
            rec.insert("rescope_error".into(), json!(e));
        }
        let _ = self.audit.write(&rec);
    }

    /// Move the daemon to `scope` (start it if stopped); sets `scope_ready`.
    pub(crate) fn rescope(&mut self, scope: LeaseScope) -> Result<(), String> {
        self.scope_ready = false;
        if self.supervisor.scope() != Some(scope) {
            self.snaps.clear();
        }
        let result = if self.supervisor.pid().is_none() {
            self.supervisor.start(scope).map(|_| ())
        } else {
            self.supervisor.rescope(scope).map(|_| ())
        };
        result.map_err(|e| e.to_string())?;
        self.scope_ready = true;
        Ok(())
    }

    /// Ensure the daemon serves `scope` before a call.
    pub(crate) fn ensure_scope(&mut self, scope: LeaseScope) -> Result<(), String> {
        if self.scope_ready
            && self.supervisor.scope() == Some(scope)
            && self.supervisor.pid().is_some()
        {
            return Ok(());
        }
        self.rescope(scope)
    }

    fn connect(
        &mut self,
        req: &Map<String, Value>,
        mut rec: Map<String, Value>,
        ctx: &Ctx,
        now: f64,
    ) -> Value {
        // #11460 Phase 4: a session reaches a grant only by redeeming its operator code.
        let code = req.get("code").and_then(Value::as_str);
        let grant = match (code, req.get("grant_id").and_then(Value::as_str)) {
            (Some(code), _) => match self.redeem(code, &ctx.session, now) {
                Ok(g) => g,
                Err((c, detail)) => return self.refuse(rec, c, detail),
            },
            (None, Some(g)) if self.consent.bound(&ctx.session, g) => g.to_owned(),
            (None, Some(g)) => {
                rec.insert("grant".into(), json!(g));
                return self.refuse(rec, "grant_not_bound", "redeem the operator's code first");
            }
            (None, None) => {
                return self.refuse(rec, "grant_required", "connect needs a code");
            }
        };
        let grant = grant.as_str();
        rec.insert("grant".into(), json!(grant));
        if let Err(e) = self.grants.live(grant, now) {
            return self.refuse(rec, e.code(), &e.to_string());
        }
        let was_holder = self.lease.position(&ctx.session) == Some(0);
        match self.lease.connect(&ctx.session, grant, now) {
            Err(e) => self.refuse(rec, e.code(), &e.to_string()),
            Ok(Connect::Queued { position }) => {
                rec.insert("lease".into(), json!("queued"));
                self.audited(
                    rec,
                    json!({"ok": true, "result": {"lease": "queued", "position": position, "grant": grant}}),
                )
            }
            Ok(Connect::Held) => {
                if !was_holder {
                    self.moved(self.lease.holder().cloned(), ctx);
                }
                rec.insert("lease".into(), json!("held"));
                if !self.scope_ready {
                    return self.refuse(
                        rec,
                        "rescope_failed",
                        "the daemon could not be scoped to this grant",
                    );
                }
                self.audited(
                    rec,
                    json!({"ok": true, "result": {"lease": "held", "grant": grant}}),
                )
            }
        }
    }

    fn status(&mut self, rec: Map<String, Value>, ctx: &Ctx) -> Value {
        let version = self.supervisor.running_version();
        if ctx.operator {
            return self.audited(
                rec,
                json!({"ok": true, "result": {"driver_version": version}}),
            );
        }
        let lease = match self.lease.position(&ctx.session) {
            Some(0) => json!({"lease": "held"}),
            Some(n) => json!({"lease": "queued", "position": n}),
            None => json!({"lease": "none"}),
        };
        self.audited(
            rec,
            json!({"ok": true, "result": {"driver_version": version, "lease": lease}}),
        )
    }

    fn operator(&mut self, op: &str, rec: Map<String, Value>) -> Value {
        let scope = self.supervisor.scope().or_else(|| {
            self.lease
                .holder()
                .and_then(|h| self.grants.get(&h.grant))
                .map(|g| g.scope)
        });
        self.snaps.clear();
        self.scope_ready = false;
        let result = match (op, scope) {
            ("stop", _) => self.supervisor.stop().map(|()| Value::Null),
            (_, None) => {
                return self.refuse(rec, "no_scope", "no lease holder to scope the daemon to");
            }
            ("start", Some(s)) => self.supervisor.start(s),
            (_, Some(s)) => self.supervisor.restart(s),
        };
        let reply = match result {
            Ok(v) => {
                self.scope_ready = op != "stop";
                json!({"ok": true, "result": v})
            }
            Err(e) => refusal("driver_error", &e.to_string()),
        };
        let mut rec = rec;
        rec.insert("ok".into(), reply["ok"].clone());
        self.audited(rec, reply)
    }
}
