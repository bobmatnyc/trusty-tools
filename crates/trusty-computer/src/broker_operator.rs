//! The operator ops: the consent flow, grant codes, revoke, listings and the lockout.
//!
//! Why: a grant may exist only after the operator confirmed the helper
//! dialog and typed its code; an agent that can speak to `operator.sock`
//! still cannot mint without that code (plan section 6, issue #11460).
//! What: `grant_request` validates the request, resolves the pid's real
//! bundle through the [`AppResolver`](crate::app_identity::AppResolver) and
//! refuses a mismatch (the sensitive check then reads the resolved bundle),
//! draws a six-digit code,
//! shows it in the [`ConsentDialog`] (no broker lock held) and on a confirm
//! stores a challenge for [`DIALOG_CODE_TTL`]. `grant_confirm` checks the
//! code, mints and answers a one-use `tc-` grant code valid
//! [`GRANT_CODE_TTL`]. Five bad codes in 60 s lock minting out; five bad
//! redeems lock redeeming out. A challenge dies after five bad codes.
//! `lockout` first writes the `<state>/lockout` marker (it survives a
//! launchd restart; `broker_unlock` clears it), then revokes every grant,
//! drops every code and the lease, stops the daemon and asks the broker
//! process to exit. Every step is audited.
//! Test: `tests/consent_fail_closed.rs`.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::Ordering;

use serde_json::{Map, Value, json};

use crate::app_identity::check_same;
use crate::broker::{Broker, Ctx, Inner, base, refusal};
use crate::consent::{
    self, DIALOG_CODE_TTL, DialogOutcome, GRANT_CODE_TTL, Prompt, PromptKind, STRIKE_LIMIT, Strikes,
};
use crate::grants::{DEFAULT_IDLE_TTL, DEFAULT_TTL, Grant, Grants, OperatorMint};
use crate::launcher::Launcher;
use crate::manifest::LeaseScope;

/// A parsed `grant_request`.
#[derive(Debug, Clone, PartialEq)]
pub struct GrantRequest {
    pub bundle_id: String,
    pub scope: LeaseScope,
    pub extra_tools: Vec<String>,
    pub ttl: f64,
    pub idle_ttl: f64,
    pub allow_sensitive: bool,
    /// Start time of the resolved process; set from the resolver, never the wire.
    pub started_us: u64,
}

impl GrantRequest {
    /// Parse the wire form; `Err` is `(code, detail)`.
    pub fn from_json(req: &Map<String, Value>) -> Result<GrantRequest, (&'static str, String)> {
        let bad = |d: &str| ("bad_request", d.to_owned());
        let bundle_id = req
            .get("bundle_id")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("bundle_id is required"))?;
        if !consent::valid_bundle_id(bundle_id) {
            return Err(("bad_bundle", bundle_id.to_owned()));
        }
        let pid = req
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|p| u32::try_from(p).ok())
            .ok_or_else(|| bad("pid is required"))?;
        let window_id = req
            .get("window_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| bad("window_id is required"))?;
        let extra_tools = match req.get("tools") {
            None => Vec::new(),
            Some(v) => v
                .as_array()
                .and_then(|a| a.iter().map(|t| t.as_str().map(str::to_owned)).collect())
                .ok_or_else(|| bad("tools must be a list of names"))?,
        };
        let secs = |k: &str, d: f64| req.get(k).map_or(Some(d), Value::as_f64);
        Ok(GrantRequest {
            bundle_id: bundle_id.to_owned(),
            scope: LeaseScope { pid, window_id },
            extra_tools,
            ttl: secs("ttl", DEFAULT_TTL).ok_or_else(|| bad("ttl must be seconds"))?,
            idle_ttl: secs("idle_ttl", DEFAULT_IDLE_TTL)
                .ok_or_else(|| bad("idle_ttl must be seconds"))?,
            allow_sensitive: req.get("allow_sensitive") == Some(&json!(true)),
            started_us: 0,
        })
    }
}

#[derive(Debug)]
struct Pending {
    req: GrantRequest,
    code: String,
    expires: f64,
    bad: usize,
}

/// Challenges, grant codes, strikes and session bindings.
#[derive(Debug, Default)]
pub struct ConsentState {
    pending: HashMap<String, Pending>,
    /// `tc-` code -> (grant id, expiry).
    codes: HashMap<String, (String, f64)>,
    pub(crate) mint_strikes: Strikes,
    redeem_strikes: Strikes,
    /// session -> grants it redeemed a code for.
    bound: HashMap<String, BTreeSet<String>>,
    /// Set by the lockout; every op but `status` is refused after it.
    pub(crate) locked_down: bool,
    /// The confirmed unlock dialog awaiting its code.
    pub(crate) unlock: Option<crate::broker_unlock::UnlockChallenge>,
}

impl ConsentState {
    /// True when `session` redeemed a code for `grant`.
    pub fn bound(&self, session: &str, grant: &str) -> bool {
        self.bound.get(session).is_some_and(|g| g.contains(grant))
    }

    /// Drop every unredeemed code of `grant`.
    pub(crate) fn drop_codes(&mut self, grant: &str) {
        self.codes.retain(|_, (g, _)| g != grant);
    }

    /// Forget a closed session's bindings.
    pub fn unbind(&mut self, session: &str) {
        self.bound.remove(session);
    }

    /// The grants `session` is bound to.
    pub fn grants_of(&self, session: &str) -> Vec<String> {
        self.bound
            .get(session)
            .map(|g| g.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// The operator-facing view of one grant.
pub fn grant_view(g: &Grant, now: f64) -> Value {
    json!({"id": g.id, "bundle_id": g.bundle_id, "pid": g.scope.pid, "window_id": g.scope.window_id,
        "tools": g.tools, "expires_in": (g.expires_at - now).max(0.0).round(),
        "state": g.dead_reason(now).unwrap_or("live"), "sensitive": g.sensitive})
}

impl<L: Launcher> Broker<L> {
    /// Operator op `grant_request`: validate, show the dialog, answer a challenge.
    ///
    /// Why: the dialog is the operator's confirmation and the only place the code appears.
    /// What: refuses a bad request, a pid whose resolved bundle is not the
    /// named one (`app_unresolved`, `bundle_mismatch`) and a sensitive
    /// resolved bundle before any dialog; refuses `locked_out` during a
    /// lockout; maps a denied, timed-out or failed dialog to
    /// `dialog_denied`, `dialog_timeout`, `dialog_failed`. The dialog shows
    /// the pid and the resolved app name and bundle.
    /// Test: `dialog_denied_refuses`, `dialog_timeout_refuses`,
    /// `grant_request_refuses_a_pid_of_another_app`, `sensitive_check_reads_the_resolved_bundle`.
    pub fn grant_request(&self, req: &Map<String, Value>, ctx: &Ctx) -> Value {
        let mut rec = base(ctx, "grant_request");
        let mut parsed = match GrantRequest::from_json(req) {
            Ok(p) => p,
            Err((code, detail)) => return self.lock().refuse(rec, code, &detail),
        };
        rec.insert("bundle_id".into(), json!(parsed.bundle_id));
        // #11460: the pid, not the claim, names the app (no broker lock held).
        let app = match self.resolver().resolve(parsed.scope.pid) {
            Ok(app) => app,
            Err(e) => return self.lock().refuse(rec, "app_unresolved", &e),
        };
        rec.insert("resolved_bundle_id".into(), json!(app.bundle_id));
        if app.bundle_id != parsed.bundle_id {
            let detail = format!(
                "pid {} is {}, not {}",
                parsed.scope.pid, app.bundle_id, parsed.bundle_id
            );
            return self.lock().refuse(rec, "bundle_mismatch", &detail);
        }
        parsed.bundle_id = app.bundle_id.clone();
        parsed.started_us = app.started_us;
        let tools = match Grants::check(&OperatorMint::consented(parsed.clone())) {
            Ok(t) => t,
            Err(e) => return self.lock().refuse(rec, e.code(), &e.to_string()),
        };
        let prompt = {
            let now = self.now();
            let mut inner = self.lock();
            if inner.consent.locked_down || inner.consent.mint_strikes.locked(now) {
                return inner.refuse(rec, "locked_out", "too many bad codes; wait 60 s");
            }
            let code = match consent::six_digits() {
                Ok(c) => c,
                Err(e) => return inner.refuse(rec, "random_failed", &e.to_string()),
            };
            Prompt {
                kind: PromptKind::Grant,
                bundle_id: app.bundle_id,
                app_name: app.name,
                pid: parsed.scope.pid,
                tools: tools.into_iter().collect(),
                ttl_secs: parsed.ttl as u64,
                code,
            }
        };
        let code = prompt.code.clone();
        let outcome = consent::ask(self.dialog(), prompt, self.dialog_timeout());
        let now = self.now();
        let mut inner = self.lock();
        let (refused, detail) = match outcome {
            DialogOutcome::Confirmed => ("", String::new()),
            DialogOutcome::Denied => ("dialog_denied", "the operator denied the grant".into()),
            DialogOutcome::TimedOut => ("dialog_timeout", "no answer in the dialog".into()),
            DialogOutcome::Failed(e) => ("dialog_failed", e),
        };
        if !refused.is_empty() {
            return inner.refuse(rec, refused, &detail);
        }
        let challenge = match consent::challenge_id() {
            Ok(c) => c,
            Err(e) => return inner.refuse(rec, "random_failed", &e.to_string()),
        };
        rec.insert("dialog".into(), json!("confirmed"));
        let reply = inner.audited(rec, json!({"ok": true, "result": {"challenge": challenge}}));
        if reply["ok"] == json!(true) {
            let pending = Pending {
                req: parsed,
                code,
                expires: now + DIALOG_CODE_TTL,
                bad: 0,
            };
            inner.consent.pending.insert(challenge, pending);
        }
        reply
    }

    /// Operator op `lockout`: the kill switch.
    ///
    /// What: writes the lockout marker first, so it holds even when a later
    /// step fails; revokes every grant (persisted), drops every challenge,
    /// code, binding, journal, snapshot and the lease, stops the daemon, and
    /// sets the shutdown flag the broker bin exits on.
    /// Test: `lockout_revokes_everything_and_stops`, `lockout_survives_a_broker_restart`,
    /// `lockout_marker_is_written_when_revoke_all_fails`.
    pub fn lockout(&self, ctx: &Ctx) -> Value {
        let mut inner = self.lock();
        let marker = crate::broker_unlock::write_marker(&inner.lockout_marker);
        inner.consent = ConsentState {
            locked_down: true,
            ..ConsentState::default()
        };
        let revoked = inner.grants.revoke_all();
        inner.journals.clear();
        inner.snaps.clear();
        inner.lease.clear();
        inner.scope_ready = false;
        let stopped = inner.supervisor.stop();
        self.shutdown.store(true, Ordering::SeqCst);
        let mut rec = base(ctx, "lockout");
        rec.insert("grants_persisted".into(), json!(revoked.is_ok()));
        rec.insert("daemon_stopped".into(), json!(stopped.is_ok()));
        rec.insert("marker_persisted".into(), json!(marker.is_ok()));
        let _ = inner.audit.write(&rec);
        json!({"ok": true, "result": {"revoked": true, "daemon_stopped": stopped.is_ok(),
            "grants_persisted": revoked.is_ok(), "marker_persisted": marker.is_ok()}})
    }

    /// True once `lockout` ran; the broker bin exits.
    pub fn shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
}

impl<L: Launcher> Inner<L> {
    /// Operator op `grant_confirm`: check the dialog code, mint, answer a `tc-` code.
    ///
    /// A pid that is no longer the confirmed process (bundle or start time)
    /// is refused `app_changed` and the challenge is dropped.
    /// Test: `wrong_code_refuses_and_locks_out`, `expired_code_refuses`,
    /// `app_changed_between_request_and_confirm_refuses`.
    pub(crate) fn grant_confirm(
        &mut self,
        req: &Map<String, Value>,
        mut rec: Map<String, Value>,
        now: f64,
    ) -> Value {
        if self.consent.mint_strikes.locked(now) {
            return self.refuse(rec, "locked_out", "too many bad codes; wait 60 s");
        }
        let challenge = req.get("challenge").and_then(Value::as_str).unwrap_or("");
        let code = req.get("code").and_then(Value::as_str).unwrap_or("");
        let Some(pending) = self.consent.pending.get_mut(challenge) else {
            self.consent.mint_strikes.strike(now);
            return self.refuse(rec, "bad_challenge", "no such challenge");
        };
        if now >= pending.expires {
            self.consent.pending.remove(challenge);
            return self.refuse(rec, "code_expired", "the dialog code expired; ask again");
        }
        if !consent::same_code(code.trim(), &pending.code) {
            pending.bad += 1;
            if pending.bad >= STRIKE_LIMIT {
                self.consent.pending.remove(challenge);
            }
            self.consent.mint_strikes.strike(now);
            return self.refuse(rec, "bad_code", "the code does not match the dialog");
        }
        let Some(pending) = self.consent.pending.remove(challenge) else {
            return self.refuse(rec, "bad_challenge", "no such challenge");
        };
        // #11460: the pid must still be the process the operator confirmed.
        let (pid, bundle) = (pending.req.scope.pid, &pending.req.bundle_id);
        if let Err(e) = check_same(&*self.resolver, pid, bundle, pending.req.started_us) {
            return self.refuse(rec, "app_changed", &e);
        }
        let grant = match self.mint(OperatorMint::consented(pending.req), now) {
            Ok(g) => g,
            Err(reply) => return reply,
        };
        let tc = match consent::grant_code() {
            Ok(c) => c,
            Err(e) => return self.refuse(rec, "random_failed", &e.to_string()),
        };
        self.consent
            .codes
            .insert(tc.clone(), (grant.id.clone(), now + GRANT_CODE_TTL));
        rec.insert("grant".into(), json!(grant.id));
        self.audited(
            rec,
            json!({"ok": true, "result": {"grant": grant_view(&grant, now), "code": tc,
                "code_expires_in": GRANT_CODE_TTL}}),
        )
    }

    /// Mint (persisted), then audit `mint`; an audit failure revokes the grant.
    pub(crate) fn mint(&mut self, req: OperatorMint, now: f64) -> Result<Grant, Value> {
        let mut rec = Map::new();
        rec.insert("op".into(), json!("mint"));
        rec.insert("bundle_id".into(), json!(req.bundle_id));
        let grant = match self.grants.mint(req, now) {
            Ok(g) => g,
            Err(e) => {
                rec.insert("refused".into(), json!(e.code()));
                let _ = self.audit.write(&rec);
                return Err(refusal(e.code(), &e.to_string()));
            }
        };
        rec.insert("grant".into(), json!(grant.id));
        rec.insert("sensitive".into(), json!(grant.sensitive));
        if let Err(e) = self.audit.write(&rec) {
            let _ = self.grants.revoke(&grant.id);
            return Err(refusal("audit_failed", &e.to_string()));
        }
        Ok(grant)
    }

    /// Redeem a one-use `tc-` code for `session`; `Err` is `(code, detail)`.
    ///
    /// Test: `grant_code_is_one_use_and_redeem_locks_out`.
    pub(crate) fn redeem(
        &mut self,
        code: &str,
        session: &str,
        now: f64,
    ) -> Result<String, (&'static str, &'static str)> {
        if self.consent.redeem_strikes.locked(now) {
            return Err(("locked_out", "too many bad codes; wait 60 s"));
        }
        match self.consent.codes.remove(code) {
            Some((grant, expires)) if now < expires => {
                self.consent
                    .bound
                    .entry(session.to_owned())
                    .or_default()
                    .insert(grant.clone());
                Ok(grant)
            }
            Some(_) => Err(("code_expired", "the grant code expired")),
            None => {
                self.consent.redeem_strikes.strike(now);
                Err(("bad_code", "unknown or used grant code"))
            }
        }
    }

    /// Operator op `grants`: every grant.
    pub(crate) fn list_grants(&mut self, rec: Map<String, Value>, now: f64) -> Value {
        let rows: Vec<Value> = self.grants.iter().map(|g| grant_view(g, now)).collect();
        self.audited(rec, json!({"ok": true, "result": {"grants": rows}}))
    }

    /// Operator op `lease`: the holder and the queue.
    pub(crate) fn lease_view(&mut self, rec: Map<String, Value>) -> Value {
        let claim = |c: &crate::lease::Claim| json!({"session": c.session, "grant": c.grant});
        let holder = self.lease.holder().map(claim);
        let queue: Vec<Value> = self.lease.queue().map(claim).collect();
        self.audited(
            rec,
            json!({"ok": true, "result": {"holder": holder, "queue": queue}}),
        )
    }

    /// Revoke `id`, drop its codes and lease claims; the lease moves on.
    pub(crate) fn revoke(
        &mut self,
        id: &str,
        mut rec: Map<String, Value>,
        ctx: &Ctx,
        now: f64,
    ) -> Value {
        let result = self.grants.revoke(id);
        self.journals.remove(id);
        self.consent.drop_codes(id);
        let moved = self.lease.drop_grant(id, now);
        rec.insert("grant".into(), json!(id));
        let reply = match &result {
            Ok(()) => json!({"ok": true}),
            Err(e) => refusal(e.code(), &e.to_string()),
        };
        let reply = self.audited(rec, reply);
        self.moved(moved, ctx);
        reply
    }
}
