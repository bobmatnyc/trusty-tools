//! Agent ops beyond `connect`, `release` and `call`: `grants`, `wait`, `refresh`.
//!
//! Why: the MCP tools `computer_grants`, `computer_wait` and
//! `computer_refresh` need broker ops (plan section 5, issue #11460).
//! What: `grants` lists only the grants this session redeemed a code for;
//! `wait` sleeps up to 5 s without the broker lock and keeps the lease
//! alive; `refresh` restarts the daemon on the holder's grant scope and
//! drops every snapshot, so the next read re-lists the window.
//! Test: `agent_grants_wait_and_refresh`.

use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::broker::{Broker, Ctx, Inner, base};
use crate::broker_operator::grant_view;
use crate::launcher::Launcher;

/// The longest `wait`, seconds.
pub const MAX_WAIT: f64 = 5.0;

impl<L: Launcher> Broker<L> {
    /// Agent op `wait {grant_id, seconds}`: needs the lease before and after the sleep.
    pub(crate) fn wait(&self, req: &Map<String, Value>, ctx: &Ctx) -> Value {
        let rec = base(ctx, "wait");
        let grant = req.get("grant_id").and_then(Value::as_str).unwrap_or("");
        let secs = req.get("seconds").and_then(Value::as_f64).unwrap_or(1.0);
        if !(0.0..=MAX_WAIT).contains(&secs) {
            return self
                .lock()
                .refuse(rec, "bad_args", "seconds must be 0 to 5");
        }
        for _ in 0..2 {
            let now = self.now();
            let mut inner = self.lock();
            if let Err(e) = inner.lease.check(&ctx.session, grant, now) {
                return inner.refuse(rec, e.code(), &e.to_string());
            }
            drop(inner);
            std::thread::sleep(Duration::from_secs_f64(secs / 2.0));
        }
        self.lock()
            .audited(rec, json!({"ok": true, "result": {"waited": secs}}))
    }
}

impl<L: Launcher> Inner<L> {
    /// Agent op `grants`: this session's grants only.
    pub(crate) fn session_grants(&mut self, rec: Map<String, Value>, ctx: &Ctx, now: f64) -> Value {
        let rows: Vec<Value> = self
            .consent
            .grants_of(&ctx.session)
            .iter()
            .filter_map(|id| self.grants.get(id))
            .map(|g| grant_view(g, now))
            .collect();
        self.audited(rec, json!({"ok": true, "result": {"grants": rows}}))
    }

    /// Agent op `refresh {grant_id}`: restart the daemon on the grant's scope.
    pub(crate) fn refresh(
        &mut self,
        grant: &str,
        rec: Map<String, Value>,
        ctx: &Ctx,
        now: f64,
    ) -> Value {
        let scope = match self.grants.live(grant, now) {
            Ok(g) => g.scope,
            Err(e) => return self.refuse(rec, e.code(), &e.to_string()),
        };
        if let Err(e) = self.lease.check(&ctx.session, grant, now) {
            return self.refuse(rec, e.code(), &e.to_string());
        }
        self.snaps.clear();
        self.scope_ready = false;
        match self.supervisor.restart(scope) {
            Ok(_) => {
                self.scope_ready = true;
                self.audited(rec, json!({"ok": true, "result": {"rescoped": true}}))
            }
            Err(e) => self.refuse(rec, "rescope_failed", &e.to_string()),
        }
    }
}
