//! Validate whole requests before applying any source mutation.
use super::*;
use std::collections::BTreeSet;

fn require(valid: bool, path: &str, message: &str) -> Result<()> {
    if valid {
        Ok(())
    } else {
        Err(ExperimentError {
            code: if path.starts_with("state.") {
                "invalid_state"
            } else {
                "invalid_request"
            },
            message: message.into(),
            path: Some(path.into()),
        })
    }
}
fn date(value: &mut String, path: &str) -> Result<()> {
    require(value.ends_with('Z'), path, "timestamp must use UTC Z")?;
    let parsed = chrono::DateTime::parse_from_rfc3339(value).map_err(|e| ExperimentError {
        code: "invalid_request",
        message: e.to_string(),
        path: Some(path.into()),
    })?;
    *value = parsed
        .with_timezone(&chrono::Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
    Ok(())
}
fn optional(value: &mut Option<String>, path: &str) -> Result<()> {
    if let Some(value) = value {
        date(value, path)?;
    }
    Ok(())
}
fn drawer(d: &mut DrawerInput) -> Result<()> {
    require(
        !d.id.is_empty() && !d.scope.is_empty(),
        "drawer.id",
        "empty identity",
    )?;
    require(
        d.importance.is_finite() && (0.0..=1.0).contains(&d.importance),
        "drawer.importance",
        "importance outside [0,1]",
    )?;
    date(&mut d.created_at, "drawer.created_at")?;
    optional(&mut d.effective_at, "drawer.effective_at")?;
    optional(&mut d.verified_at, "drawer.verified_at")?;
    optional(&mut d.expires_at, "drawer.expires_at")?;
    optional(&mut d.valid_to, "drawer.valid_to")?;
    if let (Some(start), Some(end)) = (&d.effective_at, &d.valid_to) {
        require(
            instant(end) > instant(start),
            "drawer.valid_to",
            "invalid validity interval",
        )?;
    }
    d.tags.sort();
    d.tags.dedup();
    d.aliases.sort();
    d.aliases.dedup();
    for link in &mut d.links {
        require(
            !link.target_id.is_empty() && !link.predicate.is_empty(),
            "drawer.links",
            "empty link identifier",
        )?;
        optional(&mut link.valid_from, "link.valid_from")?;
        optional(&mut link.valid_to, "link.valid_to")?;
        if let (Some(start), Some(end)) = (&link.valid_from, &link.valid_to) {
            require(
                instant(end) > instant(start),
                "link.valid_to",
                "invalid link interval",
            )?;
        }
    }
    d.links.sort();
    d.links.dedup();
    Ok(())
}
pub fn normalize_validate(r: &mut Request) -> Result<()> {
    if r.schema_version != 1 {
        return Err(ExperimentError::new(
            "unsupported_version",
            "request schema",
        ));
    }
    require(
        !r.request_id.is_empty(),
        "request_id",
        "empty request identity",
    )?;
    require(
        (1..=256).contains(&r.policy.context_tokens)
            && (32..=1024).contains(&r.policy.chunk_tokens),
        "policy",
        "invalid token budgets",
    )?;
    require(
        r.policy.freshness_weight.is_finite()
            && (0.0..=0.20).contains(&r.policy.freshness_weight)
            && r.policy.kg_weight.is_finite()
            && (0.0..=0.30).contains(&r.policy.kg_weight),
        "policy",
        "invalid ranking weights",
    )?;
    date(&mut r.as_of, "as_of")?;
    for mutation in &mut r.mutations {
        match mutation {
            Mutation::Upsert {
                revision,
                drawer: d,
            } => {
                require(*revision > 0, "revision", "revision must be positive")?;
                drawer(d)?;
            }
            Mutation::Remove {
                revision,
                scope,
                id,
            } => require(
                *revision > 0 && !scope.is_empty() && !id.is_empty(),
                "mutation",
                "invalid remove",
            )?,
        }
    }
    let mut query_ids = BTreeSet::new();
    for query in &mut r.queries {
        require(
            !query.id.is_empty() && !query.scope.is_empty() && query_ids.insert(query.id.clone()),
            "query.id",
            "invalid/duplicate query identity",
        )?;
        require(
            (1..=100).contains(&query.top_k),
            "query.top_k",
            "top_k outside [1,100]",
        )?;
        date(&mut query.as_of, "query.as_of")?;
        optional(&mut query.knowledge_cutoff, "query.knowledge_cutoff")?;
    }
    let Some(s) = &mut r.state else {
        return Ok(());
    };
    if s.schema_version != 1 || s.policy_version != VERSION {
        return Err(ExperimentError::new(
            "unsupported_version",
            "state schema/policy version",
        ));
    }
    require(
        s.treatment == r.treatment && s.policy == r.policy,
        "state.policy",
        "state treatment/policy mismatch",
    )?;
    let mut sources = BTreeSet::new();
    for source in &mut s.sources {
        drawer(&mut source.drawer)?;
        require(
            source.revision > 0
                && sources.insert(identity(&source.drawer.scope, &source.drawer.id)),
            "state.sources",
            "invalid/duplicate source",
        )?;
    }
    let mut tombstones = BTreeSet::new();
    for tomb in &s.tombstones {
        let key = identity(&tomb.scope, &tomb.id);
        require(
            tomb.revision > 0
                && !tomb.scope.is_empty()
                && !tomb.id.is_empty()
                && !sources.contains(&key)
                && tombstones.insert(key),
            "state.tombstones",
            "invalid tombstone",
        )?;
    }
    let mut docs = BTreeSet::new();
    for doc in &s.snapshot {
        doc_identity(&doc.doc_id)?;
        require(
            docs.insert(doc.doc_id.clone()),
            "state.snapshot",
            "duplicate snapshot row",
        )?;
    }
    let mut derived = BTreeSet::new();
    for row in &s.derived {
        require(
            docs.contains(&row.doc_id)
                && derived.insert(row.doc_id.clone())
                && doc_identity(&row.doc_id)? == identity(&row.scope, &row.id)
                && row.revision > 0
                && row.byte_start <= row.byte_end
                && row.line_start > 0
                && row.line_end >= row.line_start,
            "state.derived",
            "invalid derived row",
        )?;
        for hash in [&row.body_digest, &row.fingerprint] {
            require(
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "state.derived",
                "invalid digest",
            )?;
        }
        if let Some(source) = s
            .sources
            .iter()
            .find(|source| source.drawer.scope == row.scope && source.drawer.id == row.id)
        {
            require(
                row.revision <= source.revision,
                "state.derived",
                "derived revision is ahead of source",
            )?;
            if row.revision == source.revision {
                let body = &source.drawer.body;
                require(
                    row.body_digest == digest(body.as_bytes())
                        && body.get(row.byte_start..row.byte_end).is_some(),
                    "state.derived",
                    "invalid current source range/digest",
                )?;
            }
        }
    }
    if let Some(cursor) = &s.cursor {
        let (scope, id): (String, String) = serde_json::from_str(cursor)
            .map_err(|_| ExperimentError::new("invalid_state", "invalid cursor"))?;
        require(
            !scope.is_empty() && !id.is_empty(),
            "state.cursor",
            "invalid cursor",
        )?;
    }
    let sources = maintain::source_map(s);
    for source in sources.values() {
        let fingerprint = fingerprint(source, s.treatment, &s.policy, &sources)?;
        let rows: Vec<_> = s
            .derived
            .iter()
            .filter(|row| row.scope == source.drawer.scope && row.id == source.drawer.id)
            .collect();
        if rows
            .iter()
            .any(|row| row.revision == source.revision && row.fingerprint == fingerprint)
        {
            let (expected_docs, expected_rows, _) =
                derive::rows(source, s.treatment, &s.policy, &fingerprint);
            require(
                rows.len() == expected_rows.len()
                    && expected_rows
                        .iter()
                        .all(|expected| rows.contains(&expected)),
                "state.derived",
                "incomplete or inconsistent published generation",
            )?;
            require(
                s.snapshot
                    .iter()
                    .filter(|row| {
                        doc_identity(&row.doc_id).ok().as_ref()
                            == Some(&identity(&source.drawer.scope, &source.drawer.id))
                    })
                    .count()
                    == expected_docs.len()
                    && expected_docs
                        .iter()
                        .all(|expected| s.snapshot.iter().any(|row| row == expected)),
                "state.snapshot",
                "document does not match its derived generation",
            )?;
        }
    }
    Ok(())
}
