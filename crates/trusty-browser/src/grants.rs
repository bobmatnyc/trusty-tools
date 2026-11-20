//! Standing profile grants (`grants.py`, `codes.py`); contract in `docs/activation-codes.md`.
//!
//! Why: one Connect click grants a profile to every same-user local agent
//! until the browser closes or the owner disconnects (#11239 owner rulings).
//! Grants live only in memory, so parity means wire shape and audit lines.
//! What: connect, attach, list, revoke, the 45 s reconnect grace with a
//! deadline sweep, rebinding across a worker restart, and the best-effort
//! `grant_end` notice to the worker.
//! Test: `captured_exchanges_replay_byte_identically` (connect, grants,
//! profiles, attach, operator grants and revoke) in `tests/parity_broker.rs`;
//! `grace_expiry_ends_a_grant_whose_worker_never_returns` in `tests/broker_grants.rs`.

use std::sync::Arc;
use std::time::Duration;

use crate::broker::{Core, Inner};
use crate::entropy::token_hex;
use crate::pairing::{self, Fault, Route};
use crate::peer_conn::Peer;
use crate::pyjson::Json;
use crate::refusal::{Refusal, request_string};

/// Seconds a grant waits for its worker to return (`RECONNECT_GRACE`).
pub const RECONNECT_GRACE: f64 = 45.0;
/// Popup and operator grant ops.
pub const OPERATOR_OPS: &[&str] = &["grants", "revoke_grant", "revoke_grants"];
/// Refusals relayed whole on `enrollment.sock`.
pub const RELAYED: &[&str] = &[
    "InvalidRequest",
    "NotEnrolled",
    "BrowserDisconnected",
    "AuditUnavailable",
    "UnknownGrant",
];

/// One standing grant (`codes.Grant`).
#[derive(Debug, Clone)]
pub struct Grant {
    /// `g-` + 12 hex.
    pub ident: String,
    /// Always `profile` for Connect grants.
    pub scope: String,
    /// The route the grant was minted from.
    pub route: Route,
    /// Tab of a tab-scoped grant.
    pub tab: Option<i64>,
    /// Peer generation the grant is bound to.
    pub generation: Option<String>,
    /// `active` until it ends.
    pub active: bool,
    /// Attached agent session ids.
    pub sessions: Vec<u64>,
    /// Grant clock at the last admitted op.
    pub last_used: f64,
    /// Why it ended.
    pub reason: Option<String>,
    /// Browser incarnation it may rebind to.
    pub incarnation: Option<String>,
    /// End of the reconnect grace, while the worker is away.
    pub grace_until: Option<f64>,
}

/// `urlsplit(url).hostname`: lowercase host, no userinfo, port or brackets.
pub fn hostname(url: Option<&str>) -> Option<String> {
    let url = url?;
    let rest = &url[url.find("://").map_or(url.len(), |i| i + 3).min(url.len())..];
    if !url.contains("://") {
        return None;
    }
    let netloc = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let host = if let Some(inner) = host.strip_prefix('[') {
        inner.split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    (!host.is_empty()).then(|| host.to_lowercase())
}

impl Grant {
    /// Audit fields: never a key or a URL path.
    pub fn fields(&self) -> Vec<(&'static str, Option<Json>)> {
        vec![
            ("grant", Some(Json::from(self.ident.as_str()))),
            ("scope", Some(Json::from(self.scope.as_str()))),
            ("profile", Some(Json::from(self.route.profile.as_str()))),
            ("tab", self.tab.map(Json::Int)),
            (
                "origin",
                hostname(self.route.url.as_deref()).map(Json::from),
            ),
        ]
    }

    /// The listing entry.
    pub fn entry(&self, now: f64) -> Json {
        Json::object([
            ("grant", Json::from(self.ident.as_str())),
            ("scope", Json::from(self.scope.as_str())),
            ("profile", Json::from(self.route.profile.as_str())),
            (
                "state",
                Json::from(if self.active { "active" } else { "ended" }),
            ),
            ("agents", Json::Int(self.sessions.len() as i64)),
            (
                "idle_s",
                Json::Float(((now - self.last_used) * 1000.0).round() / 1000.0),
            ),
        ])
    }
}

/// `fields` plus extra ones, later keys overriding earlier ones in place.
pub fn with(
    mut base: Vec<(&'static str, Option<Json>)>,
    extra: Vec<(&'static str, Option<Json>)>,
) -> Vec<(&'static str, Option<Json>)> {
    for (k, v) in extra {
        match base.iter_mut().find(|(n, _)| *n == k) {
            Some(slot) => slot.1 = v,
            None => base.push((k, v)),
        }
    }
    base
}

fn ok_frame(pairs: Vec<(&str, Json)>) -> Json {
    let mut out = Json::object([("ok", Json::Bool(true))]);
    for (k, v) in pairs {
        out.insert(k, v);
    }
    out
}

/// `GrantEnded` with the grant's reason.
pub fn gone(grant: &Grant) -> Refusal {
    Refusal::new("GrantEnded").with("reason", grant.reason.as_deref().unwrap_or("revoked"))
}

impl Core {
    /// End every grant whose reconnect grace ran out (`expire`).
    pub fn expire(&self, inner: &mut Inner, now: f64) {
        let due: Vec<String> = inner
            .grants
            .iter()
            .filter(|g| g.active && g.grace_until.is_some_and(|t| t <= now))
            .map(|g| g.ident.clone())
            .collect();
        for ident in due {
            self.retire(inner, &ident, "browser_gone");
        }
    }

    /// Fail-closed audit line (`record`): a failed write is `AuditUnavailable`.
    pub fn record(&self, event: &str, fields: &[(&str, Option<Json>)]) -> Result<(), Refusal> {
        self.audit
            .write(event, fields)
            .map_err(|_| Refusal::new("AuditUnavailable"))
    }

    fn profile_grant<'a>(inner: &'a Inner, profile: &str) -> Option<&'a Grant> {
        inner
            .grants
            .iter()
            .find(|g| g.active && g.route.profile == profile)
    }

    /// Create or reuse the profile's standing grant (`connect`).
    ///
    /// Why: Connect is the popup's only way to create a grant.
    /// What: needs the profile's paired worker (`BrowserDisconnected`), reuses
    /// a live grant, and writes the fail-closed `connect` line before the grant exists.
    /// Test: `captured_exchanges_replay_byte_identically`.
    pub fn connect(&self, inner: &mut Inner, route: &Route) -> Result<Json, Refusal> {
        let profile = route.profile.clone();
        let Some(peer) = inner.peer(&profile) else {
            return Err(Refusal::new("BrowserDisconnected"));
        };
        self.expire(inner, self.clock.now());
        let ident = match Core::profile_grant(inner, &profile) {
            Some(g) => g.ident.clone(),
            None => {
                let grant = Grant {
                    ident: format!("g-{}", token_hex(&*self.entropy, 6)),
                    scope: "profile".into(),
                    route: route.clone(),
                    tab: None,
                    generation: Some(peer.generation.clone()),
                    active: true,
                    sessions: Vec::new(),
                    last_used: self.clock.now(),
                    reason: None,
                    incarnation: Some(peer.incarnation.clone()),
                    grace_until: None,
                };
                self.record(
                    "connect",
                    &with(grant.fields(), vec![("outcome", Some("ok".into()))]),
                )?;
                let ident = grant.ident.clone();
                inner.grants.push(grant);
                ident
            }
        };
        Ok(ok_frame(vec![
            ("grant", Json::from(ident)),
            ("scope", Json::from("profile")),
            ("profile", Json::from(profile)),
        ]))
    }

    /// The connected profiles an agent may attach to.
    pub fn profiles(inner: &Inner) -> Json {
        Json::Array(
            inner
                .grants
                .iter()
                .filter(|g| g.active)
                .map(|g| {
                    Json::object([
                        ("profile", Json::from(g.route.profile.as_str())),
                        ("email", Json::from(g.route.email.as_str())),
                        ("grant", Json::from(g.ident.as_str())),
                    ])
                })
                .collect(),
        )
    }

    /// Attach an agent session to a connected profile's grant (`attach`).
    ///
    /// Why: choosing among several connected profiles must be explicit.
    /// What: only `op` and `profile` keys; none connected is `NotConnected`,
    /// several are `ProfileChoiceRequired` with the list; writes the
    /// fail-closed `attach` line. Returns the grant id and the session id.
    /// Test: `captured_exchanges_replay_byte_identically`.
    pub fn attach(&self, inner: &mut Inner, hello: &Json) -> Result<(String, u64), Refusal> {
        if pairing::keys(hello)
            .iter()
            .any(|k| *k != "op" && *k != "profile")
        {
            return Err(Refusal::new("InvalidRequest"));
        }
        self.expire(inner, self.clock.now());
        let wanted = match hello.get("profile") {
            Some(_) => Some(request_string(hello, "profile")?.to_owned()),
            None => None,
        };
        let live: Vec<usize> = (0..inner.grants.len())
            .filter(|i| {
                let g = &inner.grants[*i];
                g.active && wanted.as_ref().is_none_or(|w| &g.route.profile == w)
            })
            .collect();
        match live.as_slice() {
            [] => Err(Refusal::new("NotConnected")),
            [i] => {
                let fields = with(
                    inner.grants[*i].fields(),
                    vec![("outcome", Some("ok".into()))],
                );
                self.record("attach", &fields)?;
                inner.next_session += 1;
                let id = inner.next_session;
                inner.grants[*i].sessions.push(id);
                Ok((inner.grants[*i].ident.clone(), id))
            }
            _ => Err(Refusal::new("ProfileChoiceRequired").with("profiles", Core::profiles(inner))),
        }
    }

    /// Ids of the profile's live grants.
    pub fn live(inner: &Inner, profile: &str) -> Vec<String> {
        inner
            .grants
            .iter()
            .filter(|g| g.route.profile == profile)
            .map(|g| g.ident.clone())
            .collect()
    }

    /// Tell the profile's worker, without waiting, that a grant ended (`notify`).
    fn notify(self: &Arc<Self>, inner: &Inner, grant: &Grant, reason: &str) {
        let Some(peer) = inner.peer(&grant.route.profile) else {
            return;
        };
        let last = Core::live(inner, &grant.route.profile).is_empty();
        let (core, ident, reason) = (Arc::clone(self), grant.ident.clone(), reason.to_owned());
        let args = vec![("reason", Json::from(reason)), ("last", Json::Bool(last))];
        tokio::spawn(async move {
            let _ = peer.page(&*core.entropy, "grant_end", &ident, args).await;
        });
    }

    /// End a grant now: remove it, write `grant_end`, notify, drop its claims.
    pub fn retire(&self, inner: &mut Inner, ident: &str, reason: &str) {
        let Some(pos) = inner
            .grants
            .iter()
            .position(|g| g.ident == ident && g.active)
        else {
            return;
        };
        let mut grant = inner.grants.remove(pos);
        grant.active = false;
        grant.reason = Some(reason.to_owned());
        if !grant.sessions.is_empty() {
            inner.ended.push(grant.clone());
        }
        self.audit.best_effort(
            "grant_end",
            &with(grant.fields(), vec![("outcome", Some(reason.into()))]),
        );
        if let Some(me) = self.this.upgrade() {
            me.notify(inner, &grant, reason);
        }
        inner
            .claims
            .retain(|c| !grant.sessions.contains(&c.owner_session));
    }

    /// Keep one task at the earliest reconnect-grace deadline (`arm`).
    pub fn arm(&self, inner: &mut Inner) {
        if let Some(task) = inner.sweeper.take() {
            task.abort();
        }
        if inner.halted {
            return;
        }
        let due = inner
            .grants
            .iter()
            .filter(|g| g.active)
            .filter_map(|g| g.grace_until)
            .fold(None, |m: Option<f64>, t| Some(m.map_or(t, |m| m.min(t))));
        let (Some(due), Some(me)) = (due, self.this.upgrade()) else {
            return;
        };
        let delay = (due - self.clock.now()).max(0.0);
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs_f64(delay)).await;
            let mut inner = me.lock();
            if inner.halted {
                return;
            }
            inner.sweeper = None;
            me.expire(&mut inner, me.clock.now());
            me.arm(&mut inner);
        });
        inner.sweeper = Some(task.abort_handle());
    }

    /// Start the reconnect grace for the lost peer's grants (`peer_gone`).
    pub fn peer_gone(&self, inner: &mut Inner, peer: &Peer) {
        let until = self.clock.now() + RECONNECT_GRACE;
        let mine: Vec<(String, bool)> = inner
            .grants
            .iter()
            .filter(|g| g.active && g.generation.as_deref() == Some(peer.generation.as_str()))
            .map(|g| {
                (
                    g.ident.clone(),
                    g.incarnation.as_deref().is_some_and(|i| !i.is_empty()),
                )
            })
            .collect();
        for (ident, has_incarnation) in mine {
            if has_incarnation {
                if let Some(g) = inner.grants.iter_mut().find(|g| g.ident == ident) {
                    g.grace_until = Some(until);
                }
            } else {
                self.retire(inner, &ident, "browser_gone");
            }
        }
        self.arm(inner);
    }

    /// Rebind the profile's grants to a returning worker (`peer_back`).
    pub fn peer_back(&self, inner: &mut Inner, peer: &Peer) {
        let now = self.clock.now();
        let mine: Vec<Grant> = inner
            .grants
            .iter()
            .filter(|g| {
                g.route.profile == peer.profile
                    && g.active
                    && g.generation
                        .as_deref()
                        .is_some_and(|x| x != peer.generation)
            })
            .cloned()
            .collect();
        for grant in mine {
            if grant.grace_until.is_some_and(|t| t <= now) {
                self.retire(inner, &grant.ident, "browser_gone");
                continue;
            }
            let same = grant
                .incarnation
                .as_deref()
                .is_some_and(|i| !i.is_empty() && i == peer.incarnation);
            if !same {
                self.retire(inner, &grant.ident, "browser_gone");
                continue;
            }
            let line = with(grant.fields(), vec![("outcome", Some("ok".into()))]);
            if self.audit.write("grant_reuse", &line).is_err() {
                self.retire(inner, &grant.ident, "browser_gone");
                continue;
            }
            if let Some(g) = inner.grants.iter_mut().find(|g| g.ident == grant.ident) {
                g.generation = Some(peer.generation.clone());
                g.grace_until = None;
            }
        }
        self.arm(inner);
    }

    /// End every grant of a profile as `unenrolled`.
    pub fn end_profile(&self, inner: &mut Inner, profile: &str) {
        let idents = Core::live(inner, profile);
        for ident in idents {
            self.retire(inner, &ident, "unenrolled");
        }
    }

    /// Detach one agent; the grant stays (`close_session`).
    pub fn close_session(&self, inner: &mut Inner, grant: &str, session: u64) {
        if let Some(g) = inner.grants.iter_mut().find(|g| g.ident == grant) {
            if let Some(pos) = g.sessions.iter().position(|s| *s == session) {
                g.sessions.remove(pos);
                let fields = with(g.fields(), vec![("outcome", Some("ok".into()))]);
                self.audit.best_effort("detach", &fields);
            }
            return;
        }
        // An ended grant is kept only while a session still names it.
        for g in &mut inner.ended {
            if g.ident == grant {
                g.sessions.retain(|s| *s != session);
            }
        }
        inner.ended.retain(|g| !g.sessions.is_empty());
    }

    /// Before an op: the worker and the pairing behind the grant must still stand.
    pub fn check_live(&self, inner: &mut Inner, ident: &str) -> Result<(), Fault> {
        let Some(grant) = inner.grant(ident).cloned() else {
            return Ok(());
        };
        let route = pairing::profile_row(&inner.db, &grant.route.profile)?;
        let peer = inner.peer(&grant.route.profile);
        if peer
            .as_ref()
            .is_none_or(|p| Some(p.generation.as_str()) != grant.generation.as_deref())
        {
            if grant.grace_until.is_some_and(|t| self.clock.now() < t) {
                return Err(Refusal::new("BrowserDisconnected").into());
            }
            self.retire(inner, ident, "browser_gone");
        } else if route.is_none_or(|r| {
            r.ticket != grant.route.ticket || r.browser_key != grant.route.browser_key
        }) {
            self.retire(inner, ident, "unenrolled");
        }
        Ok(())
    }

    /// List or revoke grants; `profile` narrows to one profile (`grant_operator`).
    ///
    /// Why: Disconnect must end access at once and the audit log must record it.
    /// What: a revoke ends each grant first, then writes the best-effort `revoke` line.
    /// Test: `captured_exchanges_replay_byte_identically`.
    pub fn grant_operator(
        &self,
        inner: &mut Inner,
        request: &Json,
        profile: Option<&str>,
    ) -> Result<Json, Refusal> {
        let op = request.get("op").and_then(Json::as_str);
        let now = self.clock.now();
        self.expire(inner, now);
        let mine: Vec<Grant> = inner
            .grants
            .iter()
            .filter(|g| profile.is_none_or(|p| g.route.profile == p))
            .cloned()
            .collect();
        let only_op = pairing::key_set_is(request, &["op"]);
        let targets = match op {
            Some("grants") if only_op => {
                let now = self.clock.now();
                let list = mine.iter().map(|g| g.entry(now)).collect();
                return Ok(ok_frame(vec![("grants", Json::Array(list))]));
            }
            Some("revoke_grants") if only_op => mine,
            Some("revoke_grant") if pairing::key_set_is(request, &["op", "grant"]) => {
                let wanted = request_string(request, "grant")?;
                match mine.into_iter().find(|g| g.ident == wanted) {
                    Some(g) => vec![g],
                    None => {
                        return Ok(Json::object([
                            ("ok", Json::Bool(false)),
                            ("error", Json::from("UnknownGrant")),
                        ]));
                    }
                }
            }
            _ => return Err(Refusal::new("InvalidRequest")),
        };
        for grant in &targets {
            self.retire(inner, &grant.ident, "revoked");
            let line = with(grant.fields(), vec![("outcome", Some("ok".into()))]);
            self.audit.best_effort("revoke", &line);
        }
        Ok(ok_frame(vec![("revoked", Json::Int(targets.len() as i64))]))
    }

    /// The popup's connect, list and revoke inside one credential's profile.
    pub fn popup_grants(&self, inner: &mut Inner, request: &Json) -> Result<Json, Fault> {
        let op = request.get("op").and_then(Json::as_str);
        let fields = pairing::triple(request);
        let (Some(fields), true) = (
            fields,
            op.is_some_and(|o| o == "connect" || OPERATOR_OPS.contains(&o)),
        ) else {
            return Err(Refusal::new("InvalidRequest").into());
        };
        let Some(route) = pairing::credential_row(&inner.db, &self.vault, &fields)? else {
            return Err(Refusal::new("NotEnrolled").into());
        };
        let rest = match request {
            Json::Object(pairs) => Json::Object(
                pairs
                    .iter()
                    .filter(|(k, _)| !pairing::CREDENTIAL.contains(&k.as_str()))
                    .cloned()
                    .collect(),
            ),
            _ => Json::Null,
        };
        if op == Some("connect") {
            if !pairing::key_set_is(&rest, &["op"]) {
                return Err(Refusal::new("InvalidRequest").into());
            }
            return Ok(self.connect(inner, &route)?);
        }
        Ok(self.grant_operator(inner, &rest, Some(&route.profile))?)
    }
}

#[cfg(test)]
mod tests {
    use super::hostname;

    #[test]
    fn hostname_matches_urlsplit() {
        assert_eq!(
            hostname(Some("https://User@Example.TEST:8443/p?q")).as_deref(),
            Some("example.test")
        );
        assert_eq!(hostname(Some("http://[::1]:80/")).as_deref(), Some("::1"));
        assert_eq!(hostname(Some("")), None);
        assert_eq!(hostname(None), None);
    }
}
