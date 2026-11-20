//! Per-profile pairing records (`pairing.py`); contract "pair" in `docs/activation-codes.md`.
//!
//! Why: a paired profile's sealed browser key is the credential behind the
//! native-port handshake and every popup op; the rules for creating,
//! replacing and removing one must match Python's exactly.
//! What: credential validation ([`triple`], [`profile_id`]), the row lookup
//! that prefers a legacy `routes` row ([`profile_row`]), credential matching
//! ([`credential_row`]), and the transactional [`pair_profile`] and
//! [`unpair_profile`].
//! Test: `captured_exchanges_replay_byte_identically` (pair, repeat, conflict,
//! invalid, unpair, same-email replace) in `tests/parity_broker.rs`.

use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior};

use crate::audit::{Audit, email_domain};
use crate::casefold::{casefold, py_strip};
use crate::entropy::Entropy;
use crate::proof::proofs_equal;
use crate::pyjson::Json;
use crate::refusal::{MAX_STRING, Refusal};
use crate::vault::Vault;

/// The credential fields of a popup frame.
pub const CREDENTIAL: [&str; 3] = ["profile", "email", "key"];

/// A broker failure that is not a protocol refusal (SQLite, vault, I/O).
#[derive(Debug, thiserror::Error)]
pub enum Fault {
    /// A refusal with its reply frame.
    #[error("{0}")]
    Refused(#[from] Refusal),
    /// Anything else; the enrollment socket answers `EnrollmentFailed`.
    #[error("{0}")]
    Internal(String),
}

impl From<rusqlite::Error> for Fault {
    fn from(e: rusqlite::Error) -> Fault {
        Fault::Internal(e.to_string())
    }
}

impl From<crate::vault::VaultError> for Fault {
    fn from(e: crate::vault::VaultError) -> Fault {
        Fault::Internal(e.to_string())
    }
}

/// A profile's route: a legacy `routes` row or a pairing in the same columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Profile id.
    pub profile: String,
    /// Case-folded email.
    pub email: String,
    /// Workflow name (routes only).
    pub workflow: Option<String>,
    /// Site URL (routes only).
    pub url: Option<String>,
    /// Account (routes only).
    pub account: Option<String>,
    /// Sealed browser key.
    pub browser_key: String,
    /// Ticket digest (routes only).
    pub ticket: Option<String>,
}

impl Route {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Route> {
        Ok(Route {
            profile: r.get(0)?,
            email: r.get(1)?,
            workflow: r.get(2)?,
            url: r.get(3)?,
            account: r.get(4)?,
            browser_key: r.get(5)?,
            ticket: r.get(6)?,
        })
    }

    /// The browser key in the clear.
    pub fn key(&self, vault: &Vault) -> Result<String, Fault> {
        Ok(vault.unseal(&format!("browser:{}", self.profile), &self.browser_key)?)
    }
}

/// `value` when it is a canonical UUID v4 string (`str(uuid.UUID(v)) == v`, version 4).
pub fn profile_id(value: Option<&Json>) -> Option<&str> {
    let s = value?.as_str()?;
    let b = s.as_bytes();
    if b.len() != 36 {
        return None;
    }
    let canonical = b.iter().enumerate().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => *c == b'-',
        _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
    });
    (canonical && b[14] == b'4' && matches!(b[19], b'8' | b'9' | b'a' | b'b')).then_some(s)
}

fn is_key(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// The well-formed `(profile, casefolded email, key)` of a frame, else `None`.
pub fn triple(request: &Json) -> Option<(String, String, String)> {
    let mut values = Vec::with_capacity(3);
    for name in CREDENTIAL {
        let v = request.get(name)?.as_str()?;
        let n = v.chars().count();
        if n == 0 || n > MAX_STRING {
            return None;
        }
        values.push(v);
    }
    let (profile, email, key) = (values[0], values[1], values[2]);
    let pid = profile_id(request.get("profile"))?;
    if pid != profile || !email.contains('@') || py_strip(email) != email || !is_key(key) {
        return None;
    }
    Some((profile.to_owned(), casefold(email), key.to_owned()))
}

/// The keys of an object frame, in order.
pub fn keys(frame: &Json) -> Vec<&str> {
    match frame {
        Json::Object(pairs) => pairs.iter().map(|(k, _)| k.as_str()).collect(),
        _ => Vec::new(),
    }
}

/// True when the frame's key set equals `want` exactly.
pub fn key_set_is(frame: &Json, want: &[&str]) -> bool {
    let have = keys(frame);
    have.len() == want.len() && want.iter().all(|w| have.contains(w))
}

const PAIR_COLUMNS: &str =
    "profile, email, NULL AS workflow, NULL AS url, NULL AS account, browser_key, NULL AS ticket";

/// The profile's enrollment route, else its pairing in the same columns.
pub fn profile_row(db: &Connection, profile: &str) -> Result<Option<Route>, Fault> {
    let route = db
        .query_row(
            "SELECT profile, email, workflow, url, account, browser_key, ticket FROM routes WHERE profile=?",
            [profile],
            Route::from_row,
        )
        .optional()?;
    if route.is_some() {
        return Ok(route);
    }
    let sql = format!("SELECT {PAIR_COLUMNS} FROM pairings WHERE profile=?");
    Ok(db.query_row(&sql, [profile], Route::from_row).optional()?)
}

/// The row whose profile, email and key all match, else `None`.
pub fn credential_row(
    db: &Connection,
    vault: &Vault,
    fields: &(String, String, String),
) -> Result<Option<Route>, Fault> {
    let (profile, email, key) = fields;
    let Some(row) = profile_row(db, profile)? else {
        return Ok(None);
    };
    if &row.email != email {
        return Ok(None);
    }
    Ok(proofs_equal(&row.key(vault)?, key).then_some(row))
}

fn conflict(audit: &Audit, reason: &str, profile: &str, email: &str) -> Fault {
    let domain = email_domain(email);
    audit.refuse("pair", reason, Some(profile), domain.as_deref());
    Fault::Refused(Refusal::new("EnrollmentConflict").with("reason", reason))
}

fn immediate(db: &Connection) -> Result<Transaction<'_>, Fault> {
    Ok(Transaction::new_unchecked(
        db,
        TransactionBehavior::Immediate,
    )?)
}

/// Seal one browser key for a profile; returns the reply and any replaced profile.
///
/// Why: a profile that lost its extension storage pairs again under a new id
/// and must not be locked out (owner ruling Dqw 2026-11-16).
/// What: an identical repeat changes nothing; one same-email pairing under
/// another id is replaced in this transaction; a route on the profile or
/// email, or a stored profile with another email or key, is
/// `EnrollmentConflict` and changes nothing. The AES-GCM nonce comes from `entropy`.
/// Test: `captured_exchanges_replay_byte_identically`.
pub fn pair_profile(
    db: &Connection,
    vault: &Vault,
    audit: &Audit,
    entropy: &dyn Entropy,
    request: &Json,
) -> Result<(Json, Option<String>), Fault> {
    let mut want = vec!["op"];
    want.extend(CREDENTIAL);
    let fields = if key_set_is(request, &want) {
        triple(request)
    } else {
        None
    };
    let (profile, email, key) = fields.ok_or_else(|| Refusal::new("InvalidEnrollment"))?;
    let tx = immediate(db)?;
    let routed = tx
        .query_row(
            "SELECT 1 FROM routes WHERE profile=? OR email=?",
            [&profile, &email],
            |_| Ok(()),
        )
        .optional()?;
    if routed.is_some() {
        return Err(conflict(audit, "enroll_conflict_route", &profile, &email));
    }
    let mut rows: Vec<(String, String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT profile, email, browser_key FROM pairings WHERE profile=? OR email=?",
        )?;
        let found = stmt.query_map([&profile, &email], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        found.collect::<Result<_, _>>()?
    };
    let mut replaced = None;
    if rows.len() == 1 && rows[0].0 != profile {
        // Only an email match reaches here: a new profile id for the same account.
        tx.execute("DELETE FROM pairings WHERE profile=?", [&rows[0].0])?;
        replaced = Some(rows[0].0.clone());
        rows.clear();
    }
    if rows.is_empty() {
        let purpose = format!("browser:{profile}");
        let sealed = vault.seal_with_nonce(&purpose, &key, &entropy.bytes(12))?;
        tx.execute(
            "INSERT INTO pairings VALUES (?,?,?)",
            [&profile, &email, &sealed],
        )?;
    } else {
        let same = rows.len() == 1 && rows[0].1 == email && {
            let stored = vault.unseal(&format!("browser:{}", rows[0].0), &rows[0].2)?;
            proofs_equal(&stored, &key)
        };
        if !same {
            return Err(conflict(audit, "enroll_conflict_key", &profile, &email));
        }
    }
    tx.commit()?;
    let reply = Json::object([("ok", Json::Bool(true)), ("profile", Json::from(profile))]);
    Ok((reply, replaced))
}

/// Delete a pairing; with a `request` its credential must match exactly.
pub fn unpair_profile(
    db: &Connection,
    vault: &Vault,
    request: Option<&Json>,
    profile: &str,
) -> Result<bool, Fault> {
    let tx = immediate(db)?;
    let present = tx
        .query_row("SELECT 1 FROM pairings WHERE profile=?", [profile], |_| {
            Ok(())
        })
        .optional()?;
    if present.is_none() {
        return Ok(false);
    }
    if let Some(request) = request {
        let Some(fields) = triple(request) else {
            return Ok(false);
        };
        if credential_row(&tx, vault, &fields)?.is_none() {
            return Ok(false);
        }
    }
    tx.execute("DELETE FROM pairings WHERE profile=?", [profile])?;
    tx.commit()?;
    Ok(true)
}

/// Every pairing as `{"profile","email"}`, ordered by email.
pub fn pairings(db: &Connection) -> Result<Json, Fault> {
    let mut stmt = db.prepare("SELECT profile, email FROM pairings ORDER BY email")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (p, e) = row?;
        out.push(Json::object([
            ("profile", Json::from(p)),
            ("email", Json::from(e)),
        ]));
    }
    Ok(Json::Array(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triple_matches_python_shape_rules() {
        let ok = r#"{"profile":"0dcdaf05-6de3-4872-9ad0-186105244c23","email":"A@Ex.Test","key":"bc1755f18c664ce836300569acb31692fca2cbfa21aeb25edf99aa8127951418"}"#;
        let frame = Json::parse(ok.as_bytes()).unwrap_or(Json::Null);
        assert_eq!(triple(&frame).map(|t| t.1).as_deref(), Some("a@ex.test"));
        for bad in [
            ok.replace("4872", "3872"),
            ok.replace("A@Ex.Test", " a@ex.test"),
            ok.replace("A@Ex.Test", "nobody"),
            ok.replace("bc17", "BC17"),
            ok.replace("0dcdaf05", "0DCDAF05"),
        ] {
            let f = Json::parse(bad.as_bytes()).unwrap_or(Json::Null);
            assert!(triple(&f).is_none(), "{bad}");
        }
    }
}
