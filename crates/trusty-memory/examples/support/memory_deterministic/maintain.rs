//! Revision-aware mutation and atomic-per-drawer bounded publication.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub fn source_map(state: &State) -> BTreeMap<String, Source> {
    state
        .sources
        .iter()
        .map(|s| (identity(&s.drawer.scope, &s.drawer.id), s.clone()))
        .collect()
}

pub fn apply(state: &mut State, mut mutations: Vec<Mutation>) -> Result<()> {
    // Coalesce order-sensitive retries: newest revisions and equal-revision
    // removals take precedence even when an old update appears first in input.
    let order = |mutation: &Mutation| match mutation {
        Mutation::Upsert { revision, drawer } => (
            identity(&drawer.scope, &drawer.id),
            std::cmp::Reverse(*revision),
            1,
        ),
        Mutation::Remove {
            revision,
            scope,
            id,
        } => (identity(scope, id), std::cmp::Reverse(*revision), 0),
    };
    mutations.sort_by_key(order);
    let mut sources = source_map(state);
    let mut tombstones: BTreeMap<_, _> = state
        .tombstones
        .iter()
        .map(|s| (identity(&s.scope, &s.id), s.clone()))
        .collect();
    for mutation in mutations {
        match mutation {
            Mutation::Upsert { revision, drawer } => {
                let key = identity(&drawer.scope, &drawer.id);
                if tombstones.get(&key).is_some_and(|t| t.revision >= revision) {
                    continue;
                }
                if let Some(old) = sources.get(&key) {
                    if old.revision > revision {
                        continue;
                    }
                    if old.revision == revision {
                        if old.drawer == *drawer {
                            continue;
                        }
                        return Err(ExperimentError::new(
                            "revision_conflict",
                            format!("conflicting revision for {key}"),
                        ));
                    }
                }
                tombstones.remove(&key);
                sources.insert(
                    key,
                    Source {
                        revision,
                        drawer: *drawer,
                    },
                );
            }
            Mutation::Remove {
                revision,
                scope,
                id,
            } => {
                let key = identity(&scope, &id);
                if sources.get(&key).is_some_and(|s| s.revision > revision)
                    || tombstones.get(&key).is_some_and(|s| s.revision > revision)
                {
                    continue;
                }
                sources.remove(&key);
                tombstones.insert(
                    key,
                    Tombstone {
                        scope,
                        id,
                        revision,
                    },
                );
            }
        }
    }
    state.sources = sources.into_values().collect();
    state.tombstones = tombstones.into_values().collect();
    Ok(())
}

pub fn fresh(
    source: &Source,
    fingerprint: &str,
    documents: &BTreeMap<String, Document>,
    derived: &BTreeMap<String, Derived>,
) -> bool {
    let id = doc_id(&source.drawer.scope, &source.drawer.id, 0);
    let matching: Vec<_> = derived
        .values()
        .filter(|r| r.scope == source.drawer.scope && r.id == source.drawer.id)
        .collect();
    derived.contains_key(&id)
        && !matching.is_empty()
        && documents
            .keys()
            .filter(|doc| {
                doc_identity(doc).ok().as_ref()
                    == Some(&identity(&source.drawer.scope, &source.drawer.id))
            })
            .count()
            == matching.len()
        && matching.iter().all(|r| {
            r.revision == source.revision
                && r.fingerprint == fingerprint
                && r.body_digest == digest(source.drawer.body.as_bytes())
                && documents.contains_key(&r.doc_id)
        })
}

pub fn maintain(state: &mut State, budget: &Budget) -> Result<Maintenance> {
    let sources = source_map(state);
    let fingerprints = sources
        .iter()
        .map(|(id, source)| {
            Ok((
                id.clone(),
                fingerprint(source, state.treatment, &state.policy, &sources)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut documents: BTreeMap<_, _> = state
        .snapshot
        .iter()
        .map(|r| (r.doc_id.clone(), r.clone()))
        .collect();
    let mut derived: BTreeMap<_, _> = state
        .derived
        .iter()
        .map(|r| (r.doc_id.clone(), r.clone()))
        .collect();
    let mut identities: BTreeSet<_> = sources.keys().cloned().collect();
    for doc in documents.values() {
        identities.insert(doc_identity(&doc.doc_id)?);
    }
    let mut ids: Vec<_> = identities.into_iter().collect();
    if let Some(cursor) = &state.cursor {
        let split = ids.partition_point(|id| id <= cursor);
        ids.rotate_left(split);
    }
    let mut report = Maintenance::default();
    if budget.max_documents > 0 && budget.max_bytes > 0 {
        for id in ids.into_iter().take(budget.max_documents) {
            let source = sources.get(&id);
            let edge_count = source.map_or(0, |s| s.drawer.links.len());
            let current =
                source.is_some_and(|s| fresh(s, &fingerprints[&id], &documents, &derived));
            let has_rows = documents
                .keys()
                .any(|key| doc_identity(key).ok().as_ref() == Some(&id));
            let rewrite =
                source.is_some() && !current && !(state.treatment == Treatment::Raw && has_rows);
            let new_rows = if rewrite {
                source.map(|s| derive::rows(s, state.treatment, &state.policy, &fingerprints[&id]))
            } else {
                None
            };
            let bytes = new_rows.as_ref().map_or(0, |(_, _, bytes)| *bytes);
            if edge_count > budget.max_edges || bytes > budget.max_bytes {
                return Err(ExperimentError::new(
                    "budget_too_small",
                    format!("{id} requires bytes={bytes}, edges={edge_count}"),
                ));
            }
            if report.bytes + bytes > budget.max_bytes
                || report.edges + edge_count > budget.max_edges
            {
                break;
            }
            report.processed += 1;
            report.bytes += bytes;
            report.edges += edge_count;
            state.cursor = Some(id.clone());
            if new_rows.is_some() || source.is_none() {
                let old: Vec<_> = documents
                    .keys()
                    .filter(|key| doc_identity(key).ok().as_ref() == Some(&id))
                    .cloned()
                    .collect();
                for key in &old {
                    documents.remove(key);
                    derived.remove(key);
                }
                if source.is_none() {
                    report.removed += old.len();
                }
                if let Some((docs, rows, _)) = new_rows {
                    report.rewritten += 1;
                    for doc in docs {
                        documents.insert(doc.doc_id.clone(), doc);
                    }
                    for row in rows {
                        derived.insert(row.doc_id.clone(), row);
                    }
                }
            }
        }
    }
    report.total = sources.len();
    report.fresh = sources
        .iter()
        .filter(|(key, source)| fresh(source, &fingerprints[*key], &documents, &derived))
        .count();
    let obsolete: BTreeSet<_> = documents
        .keys()
        .filter_map(|id| doc_identity(id).ok())
        .filter(|id| !sources.contains_key(id))
        .collect();
    report.pending = report.total - report.fresh + obsolete.len();
    report.cursor = state.cursor.clone();
    if report.rewritten > 0 || report.removed > 0 {
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| ExperimentError::new("invalid_state", "generation overflow"))?;
    }
    state.snapshot = documents.into_values().collect();
    state.derived = derived.into_values().collect();
    Ok(report)
}
