//! Retrieval: `search.query` plus hit enrichment (SPEC-OKF-01 §5.2). Feature
//! `search-feed`.
//!
//! Why: a search hit is a chunk with fixed fields; trust labels, lifecycle
//! status and verification come only from the file (§5.5). A hit can carry
//! text of an older revision, and a historical review must never make that
//! text look verified (§1.1; owner-choices item 22 B).
//! What: [`retrieve`] reads the index root from `search.index.status`
//! (F5), sends one `search.query` scoped to the bundle, and
//! re-reads each hit's file. A hit whose text is absent from the current file
//! is kept, flagged `stale_revision` and reported Unverified. A hit for a
//! deprecated or tombstoned concept (WithdrawPending or Withdrawn, 3i A) is
//! dropped. A hit it cannot read is kept as `UntrustedExternal`. A hit under
//! the bundle's `_sources/` or `_state/` is a scope leak: `IndexScope`, and
//! no hit is returned. Slice S7 of #10746.
//! Test: `c69_stale_hit_is_flagged_and_withdrawn_hits_are_dropped`,
//! `retrieve_refuses_a_scope_leak`.

use chrono::{DateTime, FixedOffset};
use serde_json::json;

use crate::OkfError;
use crate::build::trust::TrustLabel;
use crate::bundle::Bundle;
use crate::bundle::paths::confined_file;
use crate::bundle::record::{self, VerificationState};
use crate::feed::IndexFeed;
use crate::feed::probe::query;
use crate::feed::wire::{self, Chunk};
use crate::format::concept::parse_concept;
use crate::format::fields::{Field, Staleness, Status, TrustTier, staleness, trust_tier};
use crate::format::hash::content_hash;
use crate::format::id::ConceptId;
use crate::state::read::withdraw_intended;

/// One retrieval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// The query text.
    pub text: String,
    /// The most hits to ask the daemon for.
    pub top_k: usize,
}

/// One hit with what the file says about it (§5.2).
#[derive(Debug, Clone, PartialEq)]
pub struct EnrichedHit {
    /// The concept the hit belongs to.
    pub concept: ConceptId,
    /// The chunk text as the daemon returned it.
    pub content: String,
    /// First line of the chunk.
    pub start_line: usize,
    /// Last line of the chunk.
    pub end_line: usize,
    /// The daemon's score.
    pub score: f64,
    /// Lifecycle status; `None` when absent, malformed or unreadable.
    pub status: Option<Status>,
    /// The advisory OKF trust tier from frontmatter.
    pub okf_tier: TrustTier,
    /// Staleness at the caller's `now`.
    pub staleness: Staleness,
    /// Trusty's per-store label, from the file, fail-closed.
    pub trust: TrustLabel,
    /// The hit's text does not occur in the current file (item 22 B).
    pub stale_revision: bool,
    /// The verification state, attached only for current text; otherwise
    /// Unverified.
    pub verification: VerificationState,
}

/// Search `index_id` within `bundle` and enrich each hit (SPEC-OKF-01 §4.3).
///
/// Why/What: see the module doc. `now` is the caller's instant (§4.3: no
/// system clock). Errors: a failed call is `Search`; a root that does not
/// contain the bundle is `RootMismatch`; a leak is `IndexScope`.
/// Test: `c69_stale_hit_is_flagged_and_withdrawn_hits_are_dropped`,
/// `retrieve_refuses_a_scope_leak`.
pub async fn retrieve(
    bundle: &Bundle,
    index_id: &str,
    q: &Query,
    sink: &dyn IndexFeed,
    now: DateTime<FixedOffset>,
) -> Result<Vec<EnrichedHit>, OkfError> {
    let reply = sink
        .call(wire::STATUS, json!({ "index_id": index_id }))
        .await
        .map_err(OkfError::Search)?;
    let status = wire::index_status(index_id, bundle.root(), &reply)?;
    let prefix = (!status.prefix.is_empty()).then_some(status.prefix.as_str());
    let hits = query(sink, index_id, &q.text, prefix, q.top_k, false)
        .await
        .map_err(OkfError::Search)?;
    let mut leaked = Vec::new();
    let mut out = Vec::new();
    for h in hits {
        let Some(wire_path) = h.wire_path(&status.root) else {
            continue;
        };
        let Some(rel) = status.bundle_rel(&wire_path) else {
            continue;
        };
        if rel.starts_with("_sources/") || rel.starts_with("_state/") {
            leaked.push(wire_path.clone().into());
            continue;
        }
        if let Some(hit) = enrich(bundle, rel, h, now)? {
            out.push(hit);
        }
    }
    if !leaked.is_empty() {
        return Err(OkfError::IndexScope {
            index_id: index_id.to_string(),
            leaked,
        });
    }
    Ok(out)
}

/// The enriched hit for `rel`, or `None` to drop it (not a concept file, or
/// a concept that should be out of the index).
fn enrich(
    bundle: &Bundle,
    rel: &str,
    h: Chunk,
    now: DateTime<FixedOffset>,
) -> Result<Option<EnrichedHit>, OkfError> {
    let Some(stem) = rel.strip_suffix(".md") else {
        return Ok(None);
    };
    let Ok(id) = ConceptId::parse(stem) else {
        return Ok(None);
    };
    if matches!(rel.rsplit('/').next(), Some("index.md" | "log.md")) {
        return Ok(None);
    }
    let mut hit = EnrichedHit {
        concept: id.clone(),
        content: h.content,
        start_line: h.start_line,
        end_line: h.end_line,
        score: h.score,
        status: None,
        okf_tier: TrustTier::Unverified,
        staleness: Staleness::Unknown,
        trust: TrustLabel::UntrustedExternal,
        stale_revision: true,
        verification: VerificationState::Unverified { history: 0 },
    };
    let text = confined_file(bundle.root(), rel)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok());
    let Some(text) = text else {
        return Ok(Some(hit));
    };
    let Ok((c, _)) = parse_concept(id.clone(), &text) else {
        return Ok(Some(hit));
    };
    if withdraw_intended(&c) {
        return Ok(None);
    }
    let fm = c.frontmatter();
    hit.status = match &fm.status {
        Field::Valid(s) => Some(s.clone()),
        _ => None,
    };
    hit.okf_tier = trust_tier(fm);
    hit.staleness = staleness(fm, now);
    hit.trust = TrustLabel::of_entity_text(&text);
    hit.stale_revision = hit.content.trim().is_empty() || !text.contains(&hit.content);
    let events = record::fold(record::read_lines(bundle, &id)?);
    hit.verification = if hit.stale_revision {
        VerificationState::Unverified {
            history: events.len(),
        }
    } else {
        record::state_of(&events, &content_hash(&c))
    };
    Ok(Some(hit))
}
