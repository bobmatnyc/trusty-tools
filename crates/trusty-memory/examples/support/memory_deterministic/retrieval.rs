//! Scoped BM25, explicit temporal eligibility, and bounded single-hop KG candidates.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use trusty_common::bm25::BM25Index;

fn interval(d: &DrawerInput, q: &Query) -> bool {
    let now = instant(&q.as_of);
    instant(d.effective_at.as_ref().unwrap_or(&d.created_at)) <= now
        && d.valid_to.as_ref().is_none_or(|end| now < instant(end))
}
fn allowed(d: &DrawerInput, q: &Query, temporal: bool) -> bool {
    d.scope == q.scope
        && d.expires_at
            .as_ref()
            .is_none_or(|end| instant(end) >= instant(&q.as_of))
        && q.knowledge_cutoff
            .as_ref()
            .is_none_or(|cutoff| instant(&d.created_at) <= instant(cutoff))
        && (!temporal || q.mode == Mode::General || interval(d, q))
}
fn eligible(sources: &BTreeMap<String, Source>, q: &Query, temporal: bool) -> BTreeSet<String> {
    let mut ids: BTreeSet<_> = sources
        .iter()
        .filter(|(_, s)| allowed(&s.drawer, q, temporal))
        .map(|(id, _)| id.clone())
        .collect();
    if temporal && q.mode != Mode::General {
        let mut winners: BTreeMap<&str, (&Source, String)> = BTreeMap::new();
        for id in &ids {
            let source = &sources[id];
            if let Some(slot) = source.drawer.fact_key.as_deref() {
                let priority = |s: &Source| {
                    (
                        instant(
                            s.drawer
                                .effective_at
                                .as_ref()
                                .unwrap_or(&s.drawer.created_at),
                        ),
                        instant(&s.drawer.created_at),
                        s.drawer.id.clone(),
                    )
                };
                if winners
                    .get(slot)
                    .is_none_or(|(old, _)| priority(source) > priority(old))
                {
                    winners.insert(slot, (source, id.clone()));
                }
            }
        }
        ids.retain(|id| {
            sources[id]
                .drawer
                .fact_key
                .as_deref()
                .is_none_or(|slot| winners[slot].1 == *id)
        });
    }
    ids
}
fn freshness(d: &DrawerInput, q: &Query) -> (f64, String) {
    let (date, basis) = if let Some(date) = &d.verified_at {
        (date, "verified")
    } else if let Some(date) = &d.effective_at {
        (date, "effective")
    } else {
        (&d.created_at, "created")
    };
    let age = instant(&q.as_of)
        .signed_duration_since(instant(date))
        .num_milliseconds() as f64
        / 86_400_000.0;
    let half_life = match d.kind {
        Kind::SessionEvent => 7.0,
        Kind::AgentNote => 90.0,
        Kind::Commit => 365.0,
        _ => 3650.0,
    };
    (
        if age < 0.0 {
            0.0
        } else {
            2_f64.powf(-age / half_life)
        },
        basis.into(),
    )
}
fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub fn index(state: &State) -> Result<BM25Index> {
    let mut index = BM25Index::new();
    for row in &state.snapshot {
        if !index.upsert_document_reporting(&row.doc_id, &row.text) {
            return Err(ExperimentError::new(
                "index_capacity",
                format!("BM25 rejected {}", row.doc_id),
            ));
        }
    }
    Ok(index)
}

pub fn search(state: &State, q: &Query, index: &BM25Index) -> Result<QueryResult> {
    let sources = maintain::source_map(state);
    let temporal = state.treatment >= Treatment::Temporal;
    let eligible_ids = eligible(&sources, q, temporal);
    let documents: BTreeMap<_, _> = state
        .snapshot
        .iter()
        .map(|d| (d.doc_id.clone(), d.clone()))
        .collect();
    let derived: BTreeMap<_, _> = state
        .derived
        .iter()
        .map(|d| (d.doc_id.clone(), d.clone()))
        .collect();
    let mut lexical = index.score_query_all_with_filter(&q.text, index.len(), &|doc| {
        doc_identity(doc).is_ok_and(|id| eligible_ids.contains(&id))
    });
    lexical.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut candidates: BTreeMap<String, (String, f32, usize, bool)> = BTreeMap::new();
    for (doc, score) in lexical {
        let id = doc_identity(&doc)?;
        if !candidates.contains_key(&id) {
            let rank = candidates.len() + 1;
            candidates.insert(id, (doc, score, rank, false));
        }
    }
    if state.treatment == Treatment::Kg && !q.text.trim().is_empty() {
        let mut seeds: BTreeSet<_> = candidates.keys().cloned().collect();
        let alias_seeds: Vec<_> = eligible_ids
            .iter()
            .filter(|id| {
                sources[*id]
                    .drawer
                    .aliases
                    .iter()
                    .any(|a| normalized(a) == normalized(&q.text))
            })
            .cloned()
            .collect();
        seeds.extend(alias_seeds.iter().cloned());
        let mut extra = 0;
        for seed in alias_seeds {
            if let Some(candidate) = candidates.get_mut(&seed) {
                candidate.3 = true;
            } else if extra < 16 {
                let d = &sources[&seed].drawer;
                candidates.insert(seed, (doc_id(&d.scope, &d.id, 0), 0.0, 0, true));
                extra += 1;
            }
        }
        let mut edges = Vec::new();
        for seed in seeds {
            for link in &sources[&seed].drawer.links {
                let target = identity(&q.scope, &link.target_id);
                if eligible_ids.contains(&target)
                    && link
                        .valid_from
                        .as_ref()
                        .is_none_or(|start| instant(start) <= instant(&q.as_of))
                    && link
                        .valid_to
                        .as_ref()
                        .is_none_or(|end| instant(&q.as_of) < instant(end))
                {
                    edges.push((seed.clone(), link.predicate.clone(), target));
                }
            }
        }
        edges.sort();
        edges.dedup();
        for (_, _, target) in edges.into_iter().take(32) {
            if let Some(candidate) = candidates.get_mut(&target) {
                candidate.3 = true;
            } else if extra < 16 {
                let d = &sources[&target].drawer;
                candidates.insert(target, (doc_id(&d.scope, &d.id, 0), 0.0, 0, true));
                extra += 1;
            }
        }
    }
    let current = eligible(
        &sources,
        &Query {
            mode: Mode::Current,
            ..q.clone()
        },
        true,
    );
    let mut hits = Vec::new();
    for (key, (doc, bm25_score, lexical_rank, graph)) in candidates {
        let source = &sources[&key];
        let d = &source.drawer;
        let fingerprint = fingerprint(source, state.treatment, &state.policy, &sources)?;
        let index_fresh = maintain::fresh(source, &fingerprint, &documents, &derived);
        let (start, end, line_start, line_end) = if index_fresh {
            if let Some(row) = derived.get(&doc) {
                if d.body.get(row.byte_start..row.byte_end).is_none() {
                    return Err(ExperimentError::new(
                        "invalid_state",
                        "derived range is not a source boundary",
                    ));
                }
                (row.byte_start, row.byte_end, row.line_start, row.line_end)
            } else {
                (0, d.body.len(), 1, d.body.lines().count().max(1))
            }
        } else {
            (0, d.body.len(), 1, d.body.lines().count().max(1))
        };
        let (freshness, basis) = freshness(d, q);
        let score = if temporal {
            (0.90 - state.policy.freshness_weight)
                * if lexical_rank > 0 {
                    1.0 / lexical_rank as f64
                } else {
                    0.0
                }
                + 0.10 * d.importance
                + state.policy.freshness_weight * freshness
                + if graph { state.policy.kg_weight } else { 0.0 }
        } else {
            bm25_score as f64
        };
        let mut origins = Vec::new();
        if lexical_rank > 0 {
            origins.push("bm25".into());
        }
        if graph {
            origins.push("kg".into());
        }
        hits.push(Hit {
            id: d.id.clone(),
            scope: d.scope.clone(),
            rank: 0,
            score,
            bm25_score,
            excerpt: d.body[start..end].into(),
            body_digest: digest(d.body.as_bytes()),
            revision: source.revision,
            byte_start: start,
            byte_end: end,
            line_start,
            line_end,
            created_at: d.created_at.clone(),
            effective_at: d.effective_at.clone(),
            verified_at: d.verified_at.clone(),
            expires_at: d.expires_at.clone(),
            valid_to: d.valid_to.clone(),
            status: if current.contains(&key) {
                "current"
            } else if instant(d.effective_at.as_ref().unwrap_or(&d.created_at)) > instant(&q.as_of)
            {
                "unknown"
            } else {
                "historical"
            }
            .into(),
            freshness_basis: basis,
            origins,
            index_fresh,
        });
    }
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| (&a.scope, &a.id, a.byte_start).cmp(&(&b.scope, &b.id, b.byte_start)))
    });
    hits.truncate(q.top_k);
    for (index, hit) in hits.iter_mut().enumerate() {
        hit.rank = index + 1;
    }
    Ok(QueryResult {
        id: q.id.clone(),
        hits,
    })
}
