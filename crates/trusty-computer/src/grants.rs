//! Per-app grants, minted only through the operator path and kept in `grants.json`.
//!
//! Why: an agent may drive one app only after the operator granted it, and
//! a broker restart must not drop the grant (plan section 4, "Grants").
//! What: a [`Grant`] names one bundle id and the one window it may touch
//! (pid plus window id, the bounded manifest's unit), a tool subset, a TTL
//! and an idle TTL. [`Grants::mint`] takes an [`OperatorMint`], which only
//! the consent flow builds (`broker_operator.rs`: dialog confirmed and its
//! code entered). Sensitive apps need `allow_sensitive`. Every change is
//! written 0600 and fsynced before it takes effect; a write failure refuses.
//! Test: `tests/broker_grants.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::manifest::LeaseScope;
use crate::policy::{self, Scope, Verdict};
use crate::private;

/// Tools every grant carries: observe, click, scroll (plan section 4).
pub const DEFAULT_TOOLS: [&str; 8] = [
    "list_apps",
    "list_windows",
    "get_window_state",
    "verify_state",
    "click",
    "double_click",
    "right_click",
    "scroll",
];
/// Default grant lifetime, seconds.
pub const DEFAULT_TTL: f64 = 30.0 * 60.0;
/// Default grant idle lifetime, seconds.
pub const DEFAULT_IDLE_TTL: f64 = 5.0 * 60.0;
/// Browsers, terminals, Finder, System Settings and password managers.
pub const SENSITIVE_BUNDLES: [&str; 16] = [
    "com.apple.Safari",
    "com.google.Chrome",
    "org.mozilla.firefox",
    "com.microsoft.edgemac",
    "com.brave.Browser",
    "company.thebrowser.Browser",
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "dev.warp.Warp-Stable",
    "com.apple.finder",
    "com.apple.systempreferences",
    "com.apple.keychainaccess",
    "com.1password.1password",
    "com.agilebits.onepassword7",
    "com.bitwarden.desktop",
    "com.apple.Passwords",
];

/// True for a [`SENSITIVE_BUNDLES`] id in any letter case (bundle ids are case-insensitive).
pub fn is_sensitive(bundle_id: &str) -> bool {
    SENSITIVE_BUNDLES
        .iter()
        .any(|s| s.eq_ignore_ascii_case(bundle_id))
}

/// Why a grant operation was refused; `code()` is the reply code.
#[derive(Debug, thiserror::Error)]
pub enum GrantError {
    /// The bundle is sensitive and `allow_sensitive` was not set.
    #[error("sensitive_app: {0} needs --allow-sensitive")]
    Sensitive(String),
    /// A requested tool is unknown or DENY.
    #[error("bad_tool: {0} cannot be granted")]
    BadTool(String),
    /// TTLs must be positive and finite.
    #[error("bad_ttl: ttl and idle_ttl must be positive")]
    BadTtl,
    /// No such grant.
    #[error("no_grant: {0}")]
    Unknown(String),
    /// The grant expired, went idle or was revoked.
    #[error("grant_expired: {0}")]
    Expired(String),
    /// `grants.json` could not be read or written.
    #[error("grant_store: {0}")]
    Store(#[from] io::Error),
}

impl GrantError {
    /// Stable reply code.
    pub fn code(&self) -> &'static str {
        match self {
            GrantError::Sensitive(_) => "sensitive_app",
            GrantError::BadTool(_) => "bad_tool",
            GrantError::BadTtl => "bad_ttl",
            GrantError::Unknown(_) => "no_grant",
            GrantError::Expired(_) => "grant_expired",
            GrantError::Store(_) => "grant_store",
        }
    }
}

/// A mint request that only the operator path can build (`non_exhaustive`:
/// no struct literal outside this crate).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OperatorMint {
    pub bundle_id: String,
    pub scope: LeaseScope,
    /// Tools added to [`DEFAULT_TOOLS`] (`type_text`, `press_key`, ...).
    pub extra_tools: Vec<String>,
    pub ttl: f64,
    pub idle_ttl: f64,
    pub allow_sensitive: bool,
    /// Start time of the resolved process (#11460 pid reuse).
    pub started_us: u64,
}

impl OperatorMint {
    /// Built only by the consent flow once the dialog code was entered (#11460, Phase 4).
    pub(crate) fn consented(req: crate::broker_operator::GrantRequest) -> OperatorMint {
        OperatorMint {
            bundle_id: req.bundle_id,
            scope: req.scope,
            extra_tools: req.extra_tools,
            ttl: req.ttl,
            idle_ttl: req.idle_ttl,
            allow_sensitive: req.allow_sensitive,
            started_us: req.started_us,
        }
    }
}

/// One grant.
#[derive(Debug, Clone, PartialEq)]
pub struct Grant {
    pub id: String,
    pub bundle_id: String,
    pub scope: LeaseScope,
    pub tools: BTreeSet<String>,
    pub expires_at: f64,
    pub idle_ttl: f64,
    /// Last call, in memory only; a restart counts as use.
    pub last_used: f64,
    pub revoked: bool,
    pub sensitive: bool,
    /// Start time of the process the grant names; a pid with another one is another app.
    pub started_us: u64,
}

impl Grant {
    /// The policy scope: this grant's pid and bundle id.
    pub fn policy_scope(&self) -> Scope {
        Scope {
            pids: BTreeSet::from([i64::from(self.scope.pid)]),
            bundle_ids: BTreeSet::from([self.bundle_id.clone()]),
        }
    }

    /// Why the grant cannot be used at `now`, if it cannot.
    pub fn dead_reason(&self, now: f64) -> Option<&'static str> {
        if self.revoked {
            Some("revoked")
        } else if now >= self.expires_at {
            Some("expired")
        } else if now - self.last_used >= self.idle_ttl {
            Some("idle")
        } else {
            None
        }
    }

    fn to_json(&self) -> Value {
        json!({"id": self.id, "bundle_id": self.bundle_id, "pid": self.scope.pid,
            "window_id": self.scope.window_id, "tools": self.tools, "expires_at": self.expires_at,
            "idle_ttl": self.idle_ttl, "revoked": self.revoked, "sensitive": self.sensitive,
            "started_us": self.started_us})
    }

    fn from_json(v: &Value, now: f64) -> Option<Grant> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        let f = |k: &str| v.get(k).and_then(Value::as_f64);
        let tools = v.get("tools")?.as_array()?;
        Some(Grant {
            id: s("id")?,
            bundle_id: s("bundle_id")?,
            scope: LeaseScope {
                pid: u32::try_from(v.get("pid")?.as_u64()?).ok()?,
                window_id: v.get("window_id")?.as_u64()?,
            },
            tools: tools
                .iter()
                .map(|t| t.as_str().map(str::to_owned))
                .collect::<Option<_>>()?,
            expires_at: f("expires_at")?,
            idle_ttl: f("idle_ttl")?,
            last_used: now,
            revoked: v.get("revoked")?.as_bool()?,
            sensitive: v.get("sensitive")?.as_bool()?,
            // #11460: a row from before the start time is 0, refused at the first call.
            started_us: v.get("started_us").and_then(Value::as_u64).unwrap_or(0),
        })
    }
}

/// The grant table and its file.
#[derive(Debug)]
pub struct Grants {
    path: PathBuf,
    grants: BTreeMap<String, Grant>,
}

fn random_id() -> io::Result<String> {
    let mut bytes = [0u8; 6];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(format!("g-{}", hex::encode(bytes)))
}

impl Grants {
    /// Load `path` (missing means empty); a malformed file is an error, never empty.
    ///
    /// Test: `grants_survive_a_broker_restart`, `malformed_grants_file_is_refused`.
    pub fn load(path: &Path, now: f64) -> Result<Grants, GrantError> {
        let mut grants = BTreeMap::new();
        match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
            Ok(meta) if !private::private_file(&meta) => {
                return Err(io::Error::other("grants.json must be an owner-only file").into());
            }
            Ok(_) => {
                let text = std::fs::read_to_string(path)?;
                let bad = || io::Error::other("grants.json is malformed");
                let rows: Value = serde_json::from_str(&text).map_err(|_| bad())?;
                for row in rows.as_array().ok_or_else(bad)? {
                    let grant = Grant::from_json(row, now).ok_or_else(bad)?;
                    grants.insert(grant.id.clone(), grant);
                }
            }
        }
        Ok(Grants {
            path: path.to_owned(),
            grants,
        })
    }

    fn persist(&self, grants: &BTreeMap<String, Grant>) -> io::Result<()> {
        let rows: Vec<Value> = grants.values().map(Grant::to_json).collect();
        let tmp = self.path.with_extension("json.tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut file = private::open_nofollow(&tmp, false)?;
        file.write_all(Value::Array(rows).to_string().as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, &self.path)
    }

    /// Mint a grant; it exists only once `grants.json` holds it.
    ///
    /// Why: grants are the operator's decision and must outlive a restart.
    /// What: refuses a sensitive bundle without `allow_sensitive`, an unknown
    /// or DENY tool, a non-positive TTL, and a failed write (the table is
    /// left unchanged).
    /// Postcondition on Ok: `get(id)` is the grant and the file lists it.
    /// Test: `mint_refuses_sensitive_apps_and_denied_tools`, `mint_write_failure_refuses`.
    pub fn mint(&mut self, req: OperatorMint, now: f64) -> Result<Grant, GrantError> {
        let tools = Grants::check(&req)?;
        let grant = Grant {
            id: random_id()?,
            sensitive: is_sensitive(&req.bundle_id),
            bundle_id: req.bundle_id,
            scope: req.scope,
            tools,
            expires_at: now + req.ttl,
            idle_ttl: req.idle_ttl,
            last_used: now,
            revoked: false,
            started_us: req.started_us,
        };
        let mut next = self.grants.clone();
        next.insert(grant.id.clone(), grant.clone());
        self.persist(&next)?;
        self.grants = next;
        Ok(grant)
    }

    /// The tool set `req` would get, or why it is refused; checked before the dialog shows.
    pub fn check(req: &OperatorMint) -> Result<BTreeSet<String>, GrantError> {
        if !req.allow_sensitive && is_sensitive(&req.bundle_id) {
            return Err(GrantError::Sensitive(req.bundle_id.clone()));
        }
        let valid = |t: f64| t.is_finite() && t > 0.0;
        if !valid(req.ttl) || !valid(req.idle_ttl) {
            return Err(GrantError::BadTtl);
        }
        let mut tools: BTreeSet<String> = DEFAULT_TOOLS.iter().map(|t| (*t).to_owned()).collect();
        for tool in &req.extra_tools {
            match policy::rule(tool) {
                Some(rule) if rule.verdict != Verdict::Deny => {
                    tools.insert(tool.clone());
                }
                _ => return Err(GrantError::BadTool(tool.clone())),
            }
        }
        Ok(tools)
    }

    /// Revoke `id`; it is refused at once even when the write fails.
    pub fn revoke(&mut self, id: &str) -> Result<(), GrantError> {
        let grant = self
            .grants
            .get_mut(id)
            .ok_or_else(|| GrantError::Unknown(id.to_owned()))?;
        grant.revoked = true;
        self.persist(&self.grants)?;
        Ok(())
    }

    /// Revoke every grant (the lockout); refused at once even when the write fails.
    pub fn revoke_all(&mut self) -> Result<(), GrantError> {
        for g in self.grants.values_mut() {
            g.revoked = true;
        }
        self.persist(&self.grants)?;
        Ok(())
    }

    /// Every grant, alive or not, in id order.
    pub fn iter(&self) -> impl Iterator<Item = &Grant> {
        self.grants.values()
    }

    /// The grant `id`, if it exists, alive or not.
    pub fn get(&self, id: &str) -> Option<&Grant> {
        self.grants.get(id)
    }

    /// The live grant `id` at `now`, or why not.
    ///
    /// Test: `expired_idle_and_revoked_grants_are_refused`.
    pub fn live(&self, id: &str, now: f64) -> Result<&Grant, GrantError> {
        let grant = self
            .grants
            .get(id)
            .ok_or_else(|| GrantError::Unknown(id.to_owned()))?;
        match grant.dead_reason(now) {
            Some(why) => Err(GrantError::Expired(format!("{id} {why}"))),
            None => Ok(grant),
        }
    }

    /// Record use of `id` at `now` (idle clock).
    pub fn touch(&mut self, id: &str, now: f64) {
        if let Some(g) = self.grants.get_mut(id) {
            g.last_used = now;
        }
    }
}
