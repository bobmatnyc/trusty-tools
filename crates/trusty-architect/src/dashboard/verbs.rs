//! The `mpm.architect.dashboard.*` bodies and the CLI's stdout.
//!
//! Why: the daemon method, the MCP tool and `tm architect dashboard` must
//! run one body per verb with one schema, limits and exit codes (DOC-82
//! LDG-3.3, #10063 PR 3). The daemon has no state of its own for the store,
//! so each call opens the [`Store`] its request names.
//! What: [`Env`] carries what the host supplies (the secret scanner and the
//! clock). [`open`] builds the store from a [`StoreSel`]: an absolute root,
//! actor `architect` by default, and for a write the [`PruneLog`] (the
//! selection's path, else the prototype's default outside the root). One
//! function per verb; a `set` without `if_version` on an existing document
//! is a conflict, as `dashboard-db.py` (`check_set_new`). [`render`] prints
//! a reply as the prototype's CLI does: `json.dump(indent=2)` in its key
//! order, `pruned` lines raw, `tally`/`footer` as their line unless JSON
//! was asked for.
//! Test: `architect_dashboard_methods_round_trip_over_the_daemon_socket`,
//! `dispatch_architect_dashboard_tool_writes_and_conflicts`,
//! `dashboard_binary_conflict_exits_1_and_writes_nothing`,
//! `dashboard_verbs_render_the_prototype_shapes`.
//!
//! # Spec References
//! - [`SPEC-LEDGER-03~draft`](docs/specs/DOC-82-portfolio-intent-ledger-dashboard.md#SPEC-LEDGER-03~draft)

use std::sync::Arc;

use serde_json::Value;

use super::canonical::{Emit, emit};
use super::error::DashError;
use super::ids::{ID_COLLECTIONS, KEEP_FREE, free_count, next_id, retire_candidates};
use super::rpc::{
    BatchRequest, DeleteRequest, FooterRequest, FsckResponse, GetRequest, IdsRequest, IdsResponse,
    LOCAL_URL, ListRequest, PrunedRequest, PrunedResponse, StoreRequest, StoreSel, ValidateRequest,
    WriteRequest,
};
use super::schema_check::SchemaDir;
use super::store::{Doc, ListQuery, Op, OpResult, PruneLog, Store, Validation};
use super::tally::{Footer, FooterLinks, Tally, footer, tally, tally_line};
use crate::clock::Clock;

/// What the host supplies to every verb.
#[derive(Clone)]
pub struct Env {
    /// The secret-shape scanner (a shape name, never the value).
    pub scan: fn(&str) -> Option<String>,
    /// Change-log, prune-log and tally time.
    pub clock: Arc<dyn Clock + Send + Sync>,
}

/// The store `sel` names; `write` adds its prune log.
///
/// # Errors
/// [`DashError::Invalid`] for a relative root; [`PruneLog::outside`]'s.
pub fn open(sel: &StoreSel, env: &Env, write: bool) -> Result<Store, DashError> {
    if !sel.root.is_absolute() {
        return Err(DashError::Invalid(format!(
            "store root {} must be absolute",
            sel.root.display()
        )));
    }
    let actor = sel.actor.clone().unwrap_or_else(|| "architect".into());
    let store = Store::new(&sel.root, sel.format)
        .with_secret_scan(env.scan)
        .with_clock(env.clock.clone())
        .with_actor(actor);
    if !write {
        return Ok(store);
    }
    let prune = match &sel.prune_log {
        Some(path) => PruneLog::outside(&sel.root, path)?,
        None => PruneLog::default_for(&sel.root)?,
    };
    Ok(store.with_archive(Box::new(prune)))
}

/// `get`.
///
/// # Errors
/// As [`Store::get`].
pub fn get(env: &Env, r: &GetRequest) -> Result<Doc, DashError> {
    open(&r.store, env, false)?.get(&r.collection, &r.id)
}

/// `list`.
///
/// # Errors
/// As [`Store::list`].
pub fn list(env: &Env, r: &ListRequest) -> Result<Vec<Doc>, DashError> {
    let query = ListQuery {
        status: r.status.clone(),
        order_by: r.order_by.clone(),
        desc: r.desc,
        limit: r.limit,
    };
    open(&r.store, env, false)?.list(&r.collection, &query)
}

/// A conflict for a `set` with no `if_version` on a document that exists.
fn refuse_blind_replace(store: &Store, coll: &str, id: &str) -> Result<(), DashError> {
    match store.get(coll, id) {
        Err(DashError::NotFound(_)) => Ok(()),
        Err(e) => Err(e),
        Ok(_) => Err(DashError::Conflict(format!(
            "{coll}/{id} already exists; pass --if-version <v> to replace it, or `ids` for a new id"
        ))),
    }
}

/// `set`.
///
/// # Errors
/// As [`Store::batch`]; [`DashError::Conflict`] for a blind replace.
pub fn set(env: &Env, r: &WriteRequest) -> Result<OpResult, DashError> {
    let store = open(&r.store, env, true)?;
    if r.if_version.is_none() {
        refuse_blind_replace(&store, &r.collection, &r.id)?;
    }
    store.set(&r.collection, &r.id, r.data.clone(), r.if_version)
}

/// `update`.
///
/// # Errors
/// As [`Store::batch`].
pub fn update(env: &Env, r: &WriteRequest) -> Result<OpResult, DashError> {
    open(&r.store, env, true)?.update(&r.collection, &r.id, r.data.clone(), r.if_version)
}

/// `delete`.
///
/// # Errors
/// As [`Store::batch`].
pub fn delete(env: &Env, r: &DeleteRequest) -> Result<OpResult, DashError> {
    open(&r.store, env, true)?.delete(&r.collection, &r.id, r.if_version)
}

/// `batch`.
///
/// # Errors
/// As [`Store::batch`]; a bad op; a blind replace.
pub fn batch(env: &Env, r: &BatchRequest) -> Result<Vec<OpResult>, DashError> {
    let store = open(&r.store, env, true)?;
    let ops = r
        .ops
        .iter()
        .map(Op::from_json)
        .collect::<Result<Vec<_>, _>>()?;
    for op in &ops {
        if let Op::Set {
            collection,
            id,
            if_version: None,
            ..
        } = op
        {
            refuse_blind_replace(&store, collection, id)?;
        }
    }
    store.batch(&ops)
}

/// `pruned`.
///
/// # Errors
/// As [`PruneLog::lines`].
pub fn pruned(env: &Env, r: &PrunedRequest) -> Result<PrunedResponse, DashError> {
    let prune = match &r.store.prune_log {
        Some(path) => PruneLog::outside(&r.store.root, path)?,
        None => PruneLog::default_for(&r.store.root)?,
    };
    open(&r.store, env, false)?;
    let lines = prune.lines(r.id.as_deref(), r.collection.as_deref())?;
    Ok(PrunedResponse { lines })
}

/// `validate`.
///
/// # Errors
/// As [`SchemaDir::load`] and [`Store::validate`].
pub fn validate(env: &Env, r: &ValidateRequest) -> Result<Validation, DashError> {
    let schemas = SchemaDir::load(&r.schemas)?;
    open(&r.store, env, false)?.validate(&schemas)
}

/// `fsck`.
///
/// # Errors
/// As [`Store::fsck`].
pub fn fsck(env: &Env, r: &StoreRequest) -> Result<FsckResponse, DashError> {
    let problems = open(&r.store, env, false)?.fsck()?;
    Ok(FsckResponse { problems })
}

/// `tally`.
///
/// # Errors
/// As [`tally`].
pub fn tally_verb(env: &Env, r: &StoreRequest) -> Result<Tally, DashError> {
    tally(&open(&r.store, env, false)?, env.clock.now_unix())
}

/// `footer`.
///
/// # Errors
/// As [`footer`].
pub fn footer_verb(env: &Env, r: &FooterRequest) -> Result<Footer, DashError> {
    let links = FooterLinks {
        local: r.local.clone().unwrap_or_else(|| LOCAL_URL.into()),
        tailnet: r.tailnet.clone(),
    };
    footer(&open(&r.store, env, false)?, env.clock.now_unix(), &links)
}

/// `ids`.
///
/// # Errors
/// [`DashError::Invalid`] for a prefix that is not one capital letter;
/// [`Store::snapshot`]'s.
pub fn ids(env: &Env, r: &IdsRequest) -> Result<IdsResponse, DashError> {
    let mut chars = r.prefix.chars();
    let (Some(p), None) = (chars.next(), chars.next()) else {
        return Err(DashError::Invalid(format!(
            "id prefix {:?} must be one capital letter",
            r.prefix
        )));
    };
    if !p.is_ascii_uppercase() {
        return Err(DashError::Invalid(format!(
            "id prefix {:?} must be one capital letter",
            r.prefix
        )));
    }
    let snap = open(&r.store, env, false)?.snapshot(&ID_COLLECTIONS)?;
    Ok(IdsResponse {
        prefix: r.prefix.clone(),
        next: next_id(&snap, p),
        free: free_count(&snap, p),
        retire: retire_candidates(&snap, p, KEEP_FREE),
    })
}

/// A reply, for [`render`].
#[derive(Debug)]
pub enum Reply {
    /// `get`.
    Doc(Doc),
    /// `list`.
    Docs(Vec<Doc>),
    /// `set`, `update`, `delete`.
    Op(OpResult),
    /// `batch`.
    Ops(Vec<OpResult>),
    /// `pruned`.
    Pruned(PrunedResponse),
    /// `validate`.
    Validation(Validation),
    /// `fsck`.
    Fsck(FsckResponse),
    /// `tally`; `true` prints the JSON.
    Tally(Tally, bool),
    /// `footer`; `true` prints the JSON.
    Footer(Footer, bool),
    /// `ids`.
    Ids(IdsResponse),
}

fn s(v: impl Into<Value>) -> Emit {
    Emit::Value(v.into())
}

fn obj(fields: Vec<(&str, Emit)>) -> Emit {
    Emit::Object(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn doc(d: &Doc) -> Emit {
    obj(vec![
        ("id", s(d.id.as_str())),
        ("version", s(d.version)),
        ("data", s(Value::Object(d.data.clone()))),
    ])
}

fn op(r: &OpResult) -> Emit {
    match r {
        OpResult::Written {
            collection,
            id,
            version,
        } => obj(vec![
            ("collection", s(collection.as_str())),
            ("id", s(id.as_str())),
            ("version", s(*version)),
        ]),
        OpResult::Deleted {
            collection,
            id,
            deleted,
        } => obj(vec![
            ("collection", s(collection.as_str())),
            ("id", s(id.as_str())),
            ("deleted", s(*deleted)),
        ]),
    }
}

fn tally_emit(t: &Tally) -> Emit {
    let mut fields = vec![
        ("open", s(t.open)),
        ("questions", s(t.questions)),
        ("actions", s(t.actions)),
        ("tasks", s(t.tasks)),
        ("answered_unrelayed", s(t.answered_unrelayed)),
        ("parked", s(t.parked)),
        ("running", s(t.running)),
    ];
    if let Some(w) = t.answers_waiting {
        fields.push(("answers_waiting", s(w)));
    }
    obj(fields)
}

fn strings(items: &[String]) -> Emit {
    Emit::List(items.iter().map(|x| s(x.as_str())).collect())
}

/// The CLI's stdout for `reply`, as `dashboard-db.py` prints it.
pub fn render(reply: &Reply) -> String {
    match reply {
        Reply::Doc(d) => emit(&doc(d)),
        Reply::Docs(ds) => emit(&Emit::List(ds.iter().map(doc).collect())),
        Reply::Op(r) => emit(&op(r)),
        Reply::Ops(rs) => emit(&Emit::List(rs.iter().map(op).collect())),
        Reply::Pruned(p) => p.lines.iter().map(|l| format!("{l}\n")).collect(),
        Reply::Validation(v) => emit(&obj(vec![
            ("errors", strings(&v.errors)),
            ("documents", s(v.documents)),
        ])),
        Reply::Fsck(f) => emit(&obj(vec![("problems", strings(&f.problems))])),
        Reply::Tally(t, false) => tally_line(t) + "\n",
        Reply::Tally(t, true) => emit(&tally_emit(t)),
        Reply::Footer(f, false) => format!("{}\n", f.footer),
        Reply::Footer(f, true) => emit(&obj(vec![
            ("footer", s(f.footer.as_str())),
            ("tally", tally_emit(&f.tally)),
        ])),
        Reply::Ids(i) => emit(&obj(vec![
            ("prefix", s(i.prefix.as_str())),
            ("next", s(i.next.clone())),
            ("free", s(i.free)),
            ("retire", strings(&i.retire)),
        ])),
    }
}

/// The CLI exit code of a `reply`: 1 when `fsck` or `validate` found a
/// problem (the prototype's), else 0.
pub fn exit_code(reply: &Reply) -> i32 {
    match reply {
        Reply::Fsck(f) if !f.problems.is_empty() => 1,
        Reply::Validation(v) if !v.errors.is_empty() => 1,
        _ => 0,
    }
}
