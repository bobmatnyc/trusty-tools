//! `Peer.command`: a worker command checked against the whole route (`protocol.py`).
//!
//! Why: the lease indicator (`lease_show`, `lease_clear`) is sent with the
//! route's `email`, `url` and `account` as expectations, which the worker
//! echoes; a mismatch means another identity answered. Unlike [`Peer::page`]
//! it relays any error name and keeps no detail fields.
//! What: draws the id, writes `{"id","op","email","url","account",...}` with
//! each expectation as Python's `str()` of the row value (`None` for NULL),
//! waits 8 s; a lost reply is `OutcomeUnknown` for `click` else
//! `BrowserDisconnected`; returns `{email,url,account,tab,result}`.
//! Test: `indicator_commands_carry_the_route_and_python_reply_rules` in
//! `src/leases.rs`.

use std::sync::PoisonError;
use std::time::Duration;

use tokio::sync::oneshot;

use super::{PAGE_WAIT, Peer};
use crate::entropy::{Entropy, token_urlsafe};
use crate::pyjson::Json;
use crate::refusal::Refusal;
use crate::wire::write_frame;

impl Peer {
    fn expectations(&self) -> [(&'static str, String); 3] {
        let text = |v: &Option<String>| v.clone().unwrap_or_else(|| "None".to_owned());
        [
            ("email", self.route.email.clone()),
            ("url", text(&self.route.url)),
            ("account", text(&self.route.account)),
        ]
    }

    /// One route-checked command (`Peer.command`).
    pub async fn command(
        &self,
        entropy: &dyn Entropy,
        op: &str,
        args: Vec<(&str, Json)>,
    ) -> Result<Json, Refusal> {
        let ident = token_urlsafe(entropy, 18);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(ident.clone(), tx);
        let expected = self.expectations();
        let mut frame = Json::object([("id", Json::from(ident.as_str())), ("op", Json::from(op))]);
        for (k, v) in &expected {
            frame.insert(*k, Json::from(v.as_str()));
        }
        let want_tab = args
            .iter()
            .find(|(k, _)| *k == "tab")
            .map(|(_, v)| v.clone());
        for (k, v) in args {
            frame.insert(k, v);
        }
        let lost = || {
            Refusal::new(if op == "click" {
                "OutcomeUnknown"
            } else {
                "BrowserDisconnected"
            })
        };
        let sent = {
            let mut w = self.writer.lock().await;
            write_frame(&mut *w, &frame).await
        };
        let observed = match sent {
            Ok(()) => tokio::time::timeout(Duration::from_secs_f64(PAGE_WAIT), rx).await,
            Err(_) => {
                self.forget_id(&ident);
                self.kill();
                return Err(lost());
            }
        };
        self.forget_id(&ident);
        let Ok(Ok(observed)) = observed else {
            self.kill();
            return Err(lost());
        };
        if observed.get("ok") != Some(&Json::Bool(true)) {
            let code = observed
                .get("error")
                .and_then(Json::as_str)
                .unwrap_or("BrowserRefused");
            return Err(Refusal::new(code));
        }
        if expected
            .iter()
            .any(|(k, v)| observed.get(k).and_then(Json::as_str) != Some(v.as_str()))
        {
            return Err(Refusal::new("IdentityMismatch"));
        }
        let tab = observed.get("tab");
        let tab_ok = matches!(tab, Some(Json::Int(t)) if *t >= 0)
            && want_tab.as_ref().is_none_or(|w| Some(w) == tab);
        if !tab_ok {
            return Err(Refusal::new("TabMismatch"));
        }
        Ok(Json::object(
            ["email", "url", "account", "tab", "result"]
                .map(|k| (k, observed.get(k).cloned().unwrap_or(Json::Null))),
        ))
    }
}
