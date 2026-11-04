//! The trusty-search method names, request bodies and reply readers the feed
//! and `retrieve` use (SPEC-OKF-01 §5.1, §5.2, §5.6 D1).
//!
//! Why: every reply is read fail-closed in one place. A reply that lacks a
//! field the contract needs is a refusal, never a default.
//! What: method constants, [`IndexStatus`] (root check F5 and the live
//! identity, §4.9.2), [`classify_put`] (F4 and the D1 hash check), and the
//! chunk and hit reader. #10746 S7.
//! Test: `c46_indexed_false_appends_nothing_and_removed_true_succeeds`,
//! `c47_root_mismatch_refuses_before_any_put`,
//! `put_with_a_wrong_content_sha256_is_not_counted`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::OkfError;
use crate::state::{IndexIdentity, LiveIdentity};

/// Preflight and identity (§5.2).
pub(crate) const STATUS: &str = "search.index.status";
/// One page of chunks under a prefix (§4.9 probe, §5.3.1 scope check).
pub(crate) const CHUNKS_LIST: &str = "search.chunks.list";
/// Scoped query (§4.9 probe, `retrieve`).
pub(crate) const QUERY: &str = "search.query";
/// Push one file (§5.2).
pub(crate) const FILE_PUT: &str = "search.index.file.put";
/// Withdraw one file (§5.2).
pub(crate) const FILE_REMOVE: &str = "search.index.file.remove";
/// Withdraw a prefix (§5.3.1 rule 3).
pub(crate) const FILES_REMOVE: &str = "search.index.files.remove";
/// Per-file revision evidence (D1, #10738).
pub(crate) const FILE_STATUS: &str = "search.index.file.status";

/// What `search.index.status` said about the index.
#[derive(Debug, Clone)]
pub(crate) struct IndexStatus {
    /// The absolute index root the daemon reported.
    pub(crate) root: PathBuf,
    /// The bundle root's path below the index root, `/`-separated; empty
    /// when the bundle root is the index root.
    pub(crate) prefix: String,
    /// The live identity (§4.9.2).
    pub(crate) live: LiveIdentity,
}

impl IndexStatus {
    /// The index-relative wire path of a bundle-relative file name.
    pub(crate) fn wire(&self, rel: &str) -> String {
        if self.prefix.is_empty() {
            rel.to_string()
        } else {
            format!("{}/{rel}", self.prefix)
        }
    }

    /// The bundle-relative name of an index-relative path, or `None` when it
    /// lies outside the bundle.
    pub(crate) fn bundle_rel<'a>(&self, wire: &'a str) -> Option<&'a str> {
        if self.prefix.is_empty() {
            return Some(wire);
        }
        wire.strip_prefix(self.prefix.as_str())?.strip_prefix('/')
    }

    /// Whether D1 revision evidence is available (§4.9 probe check 2).
    pub(crate) fn d1(&self) -> bool {
        matches!(self.live, LiveIdentity::Known(_))
    }
}

/// The live identity a `search.index.status` reply carries (§4.9.2).
///
/// What: an `identity {instance_id, generation}` object is `Known`; a null
/// identity (the D1 daemon's `identity_error` case) is `Unreadable`; no
/// `identity` key at all is `Unsupported` (D1 absent). A malformed object
/// is `Unreadable`, never a guess.
/// Test: `no_state_reads_searchable_without_d1_through_the_feed`.
pub fn live_identity(reply: &Value) -> LiveIdentity {
    match reply.get("identity") {
        None => LiveIdentity::Unsupported,
        Some(Value::Null) => LiveIdentity::Unreadable(
            reply
                .get("identity_error")
                .and_then(Value::as_str)
                .unwrap_or("identity is null")
                .to_string(),
        ),
        Some(v) => match parse_identity(v) {
            Some(i) => LiveIdentity::Known(i),
            None => LiveIdentity::Unreadable(format!("malformed identity: {v}")),
        },
    }
}

/// An identity object, or `None` when it is not exactly one.
fn parse_identity(v: &Value) -> Option<IndexIdentity> {
    let instance_id = v.get("instance_id")?.as_str()?.to_string();
    let generation = v.get("generation")?.as_u64()?;
    (!instance_id.is_empty()).then_some(IndexIdentity {
        instance_id,
        generation,
    })
}

/// Read a status reply against `bundle_root` (F5).
///
/// What: the reply's `root_path` must be absolute and must contain the
/// canonical bundle root (both sides canonicalised when they exist);
/// otherwise `RootMismatch`.
pub(crate) fn index_status(
    index_id: &str,
    bundle_root: &Path,
    reply: &Value,
) -> Result<IndexStatus, OkfError> {
    let mismatch = |index_root: Option<PathBuf>| OkfError::RootMismatch {
        index_id: index_id.to_string(),
        index_root,
        bundle_root: bundle_root.to_path_buf(),
    };
    let root = reply
        .get("root_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| mismatch(None))?;
    let canon = root.canonicalize().unwrap_or_else(|_| root.clone());
    let rel = bundle_root
        .strip_prefix(&canon)
        .map_err(|_| mismatch(Some(root.clone())))?;
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            std::path::Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            _ => return Err(mismatch(Some(root.clone()))),
        }
    }
    Ok(IndexStatus {
        root,
        prefix: parts.join("/"),
        live: live_identity(reply),
    })
}

/// How the daemon answered one `file.put`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PutVerdict {
    /// Indexed exactly the bytes sent; the identity at commit, if reported.
    Indexed(Option<IndexIdentity>),
    /// A tombstone reply with `removed: true` (F4's one zero-chunk success).
    Removed(Option<IndexIdentity>),
    /// Not a confirmation of these bytes; the reason, for the task error.
    Refused(String),
}

/// Classify a put reply against the hex SHA-256 of the bytes sent (F4, D1).
///
/// What: `removed: true` or `indexed: true` is needed; anything else is
/// refused, an absent `indexed` included. A `content_sha256` that differs
/// from `sent_hex` is refused; under D1 (`d1`) an absent one is refused too,
/// because the feed counts a put only when the hash matches (§5.6).
pub(crate) fn classify_put(reply: &Value, sent_hex: &str, d1: bool) -> PutVerdict {
    let removed = reply.get("removed").and_then(Value::as_bool) == Some(true);
    if !removed {
        match reply.get("indexed").and_then(Value::as_bool) {
            Some(true) => {}
            Some(false) => {
                return PutVerdict::Refused(format!(
                    "indexed: false ({})",
                    reply
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("no reason")
                ));
            }
            None => return PutVerdict::Refused("reply carries no indexed flag".to_string()),
        }
    }
    match reply.get("content_sha256") {
        Some(Value::String(h)) if h.eq_ignore_ascii_case(sent_hex) => {}
        Some(Value::String(h)) => {
            return PutVerdict::Refused(format!("content_sha256 {h} is not the bytes sent"));
        }
        None | Some(Value::Null) if !d1 => {}
        other => {
            return PutVerdict::Refused(format!(
                "reply carries no content_sha256 under an index identity: {other:?}"
            ));
        }
    }
    let identity = reply.get("identity").and_then(parse_identity);
    if removed {
        PutVerdict::Removed(identity)
    } else {
        PutVerdict::Indexed(identity)
    }
}

/// One chunk or hit, as the feed reads it.
#[derive(Debug, Clone)]
pub(crate) struct Chunk {
    /// Index-relative path (`path`), when the daemon reported one.
    pub(crate) path: Option<String>,
    /// Absolute path (`file`).
    pub(crate) file: Option<String>,
    /// The chunk text.
    pub(crate) content: String,
    /// First line.
    pub(crate) start_line: usize,
    /// Last line.
    pub(crate) end_line: usize,
    /// Rank score.
    pub(crate) score: f64,
}

impl Chunk {
    /// The index-relative path, from `path` or from `file` under `root`.
    pub(crate) fn wire_path(&self, root: &Path) -> Option<String> {
        if let Some(p) = &self.path {
            return Some(p.clone());
        }
        let file = Path::new(self.file.as_deref()?);
        let rel = file
            .strip_prefix(root)
            .ok()
            .or_else(|| file.strip_prefix(root.canonicalize().ok()?).ok())?;
        Some(rel.to_string_lossy().replace('\\', "/"))
    }
}

/// The chunk array under `key` (`chunks` or `results`); entries that are
/// not objects are skipped, a missing `content` reads as empty.
pub(crate) fn chunks_at(reply: &Value, key: &str) -> Vec<Chunk> {
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let n = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0) as usize;
    reply
        .get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|v| v.is_object())
                .map(|v| Chunk {
                    path: s(v, "path"),
                    file: s(v, "file"),
                    content: s(v, "content").unwrap_or_default(),
                    start_line: n(v, "start_line"),
                    end_line: n(v, "end_line"),
                    score: v.get("score").and_then(Value::as_f64).unwrap_or(0.0),
                })
                .collect()
        })
        .unwrap_or_default()
}
