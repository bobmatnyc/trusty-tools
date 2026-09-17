use super::*;
use serde_json::json;

fn drawer(id: &str, body: &str) -> DrawerInput {
    serde_json::from_value(json!({"id":id,"scope":"scope","room":"room","body":body,"created_at":"2026-01-01T00:00:00Z"})).unwrap()
}
fn request(treatment: Treatment, drawers: Vec<DrawerInput>) -> Request {
    Request {
        schema_version: 1,
        request_id: "test".into(),
        treatment,
        as_of: "2026-09-01T00:00:00Z".into(),
        state: None,
        policy: Policy::default(),
        mutations: drawers
            .into_iter()
            .map(|drawer| Mutation::Upsert {
                revision: 1,
                drawer: drawer.into(),
            })
            .collect(),
        maintenance: Budget {
            max_documents: 100,
            max_bytes: 1_000_000,
            max_edges: 100,
        },
        queries: vec![],
    }
}
fn query(text: &str) -> Query {
    Query {
        id: "q".into(),
        text: text.into(),
        scope: "scope".into(),
        mode: Mode::Current,
        as_of: "2026-09-01T00:00:00Z".into(),
        knowledge_cutoff: None,
        top_k: 5,
    }
}
fn resume(state: State) -> Request {
    Request {
        treatment: state.treatment,
        policy: state.policy.clone(),
        state: Some(state),
        ..request(Treatment::Raw, vec![])
    }
}

#[test]
fn body_metadata_repair_noop_and_source_preservation() {
    let mut d = drawer("a", "exact café original\nsecond line\n");
    d.tags = vec!["tagold".into()];
    let first = evaluate(request(Treatment::Context, vec![d.clone()])).unwrap();
    assert_eq!(first.state.sources[0].drawer.body, d.body);
    let unchanged = evaluate(resume(first.state.clone())).unwrap();
    assert_eq!(
        (
            unchanged.maintenance.rewritten,
            unchanged.maintenance.removed
        ),
        (0, 0)
    );
    assert_eq!(first.state.generation, unchanged.state.generation);
    d.tags = vec!["tagnew".into()];
    let mut changed = resume(first.state);
    changed.mutations = vec![Mutation::Upsert {
        revision: 2,
        drawer: d.clone().into(),
    }];
    changed.queries = vec![query("tagnew")];
    let second = evaluate(changed).unwrap();
    assert_eq!(second.maintenance.rewritten, 1);
    assert_eq!(second.results[0].hits.len(), 1);
    d.body = "replacement body".into();
    let mut changed = resume(second.state);
    changed.mutations = vec![Mutation::Upsert {
        revision: 3,
        drawer: d.into(),
    }];
    changed.queries = vec![query("replacement")];
    let final_state = evaluate(changed).unwrap();
    assert_eq!(final_state.results[0].hits[0].excerpt, "replacement body");
    let restarted: State =
        serde_json::from_slice(&serde_json::to_vec(&final_state.state).unwrap()).unwrap();
    assert_eq!(restarted, final_state.state);
}

#[test]
fn bounded_resume_legacy_import_and_snapshot_compatibility() {
    let mut r = request(
        Treatment::Context,
        vec![
            drawer("a", "alpha"),
            drawer("b", "bravo"),
            drawer("c", "charlie"),
        ],
    );
    r.maintenance.max_documents = 1;
    let one = evaluate(r).unwrap();
    assert_eq!((one.maintenance.fresh, one.maintenance.pending), (1, 2));
    let mut next = resume(one.state);
    next.maintenance.max_documents = 1;
    let two = evaluate(next).unwrap();
    assert_eq!(two.maintenance.fresh, 2);
    let mut next = resume(two.state);
    next.maintenance.max_documents = 1;
    let three = evaluate(next).unwrap();
    assert_eq!(three.maintenance.pending, 0);
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("bm25_index.json"),
        serde_json::to_vec(&three.state.snapshot).unwrap(),
    )
    .unwrap();
    let legacy = trusty_memory::bm25_index::PalaceBm25Index::load_or_create(temp.path()).unwrap();
    assert_eq!(legacy.search("alpha", 5)[0].doc_id, doc_id("scope", "a", 0));
    let mut state = three.state;
    state.derived.clear();
    let mut r = resume(state);
    r.maintenance.max_documents = 0;
    r.queries = vec![query("alpha")];
    let legacy = evaluate(r).unwrap();
    assert_eq!(legacy.maintenance.fresh, 0);
    assert!(!legacy.results[0].hits[0].index_fresh);
}

#[test]
fn tombstones_revisions_and_atomic_conflicts() {
    let first = evaluate(request(Treatment::RepairedRaw, vec![drawer("a", "alpha")])).unwrap();
    let mut r = resume(first.state.clone());
    r.maintenance.max_documents = 0;
    r.mutations = vec![
        Mutation::Remove {
            revision: 2,
            scope: "scope".into(),
            id: "a".into(),
        },
        Mutation::Upsert {
            revision: 1,
            drawer: drawer("a", "resurrect").into(),
        },
    ];
    r.queries = vec![query("alpha")];
    let removed = evaluate(r).unwrap();
    assert!(removed.results[0].hits.is_empty());
    let mut r = resume(removed.state);
    r.mutations = vec![Mutation::Upsert {
        revision: 2,
        drawer: drawer("a", "resurrect").into(),
    }];
    assert!(evaluate(r.clone()).unwrap().state.sources.is_empty());
    r.mutations = vec![Mutation::Upsert {
        revision: 3,
        drawer: drawer("a", "recreated").into(),
    }];
    assert_eq!(evaluate(r).unwrap().state.sources.len(), 1);
    let mut conflict = resume(first.state);
    conflict.mutations = vec![Mutation::Upsert {
        revision: 1,
        drawer: drawer("a", "conflict").into(),
    }];
    assert_eq!(evaluate(conflict).unwrap_err().code, "revision_conflict");
}

#[test]
fn temporal_scope_and_boundary_rules() {
    let mut old = drawer("old", "owner old");
    old.fact_key = Some("owner".into());
    old.valid_to = Some("2026-07-01T00:00:00Z".into());
    let mut new = drawer("new", "owner new");
    new.fact_key = old.fact_key.clone();
    new.created_at = "2026-07-01T00:00:00Z".into();
    let mut foreign = drawer("foreign", "owner owner owner");
    foreign.scope = "other".into();
    let mut expired = drawer("expired", "owner");
    expired.expires_at = Some("2026-09-01T00:00:00Z".into());
    let mut r = request(Treatment::Temporal, vec![old, new, foreign, expired]);
    r.queries = vec![query("owner")];
    let current = evaluate(r).unwrap();
    let ids: Vec<_> = current.results[0]
        .hits
        .iter()
        .map(|h| h.id.as_str())
        .collect();
    assert!(ids.contains(&"new") && ids.contains(&"expired"));
    assert!(!ids.contains(&"old") && !ids.contains(&"foreign"));
    let mut history = resume(current.state);
    let mut q = query("owner");
    q.mode = Mode::Asof;
    q.as_of = "2026-06-01T00:00:00Z".into();
    q.knowledge_cutoff = Some(q.as_of.clone());
    history.queries = vec![q];
    let hits = evaluate(history).unwrap().results.remove(0).hits;
    assert!(hits.iter().any(|h| h.id == "old"));
    assert!(hits.iter().all(|h| h.id != "new"));
}

#[test]
fn deterministic_ties_chunks_alias_links_and_stale_locators() {
    let mut seed = drawer("seed", "seed body");
    seed.aliases = vec!["Exact alias".into()];
    seed.links = vec![Link {
        target_id: "target".into(),
        predicate: "points-to".into(),
        valid_from: None,
        valid_to: None,
    }];
    let mut r = request(Treatment::Kg, vec![seed, drawer("target", "answer café")]);
    r.queries = vec![query("Exact alias")];
    let a = evaluate(r.clone()).unwrap();
    let b = evaluate(r).unwrap();
    assert_eq!(canonical(&a).unwrap(), canonical(&b).unwrap());
    assert!(a.results[0]
        .hits
        .iter()
        .any(|h| h.id == "target" && h.origins.contains(&"kg".into())));
    let body = "alpha beta\n\ngamma delta\n\népsilon zeta";
    let ranges = derive::ranges(body, 2, true);
    assert!(ranges.len() > 1);
    assert_eq!(
        ranges
            .iter()
            .map(|(s, e)| &body[*s..*e])
            .collect::<String>(),
        body
    );
    assert!(ranges
        .iter()
        .all(|(s, e)| derive::token_occurrences(&body[*s..*e]) <= 2));
    let mut r = request(
        Treatment::Raw,
        vec![drawer("b", "equal"), drawer("a", "equal")],
    );
    r.queries = vec![query("equal")];
    let mut state = evaluate(r).unwrap().state;
    state.sources[0].drawer.body = "short".into();
    state.sources[0].revision = 2;
    let mut r = resume(state);
    r.queries = vec![query("equal")];
    let answer = evaluate(r).unwrap();
    assert_eq!(answer.results[0].hits[0].id, "a");
    assert_eq!(answer.results[0].hits[0].excerpt, "short");
    assert!(!answer.results[0].hits[0].index_fresh);
}

#[test]
fn validation_errors_and_budget_retry() {
    let mut r = request(Treatment::Chunks, vec![drawer("a", "nonempty body")]);
    r.maintenance.max_bytes = 1;
    assert_eq!(evaluate(r).unwrap_err().code, "budget_too_small");
    let mut r = request(Treatment::Context, vec![drawer("a", "body")]);
    r.mutations.push(Mutation::Upsert {
        revision: 0,
        drawer: drawer("b", "invalid").into(),
    });
    assert!(evaluate(r).is_err());
    let mut r = request(Treatment::Context, vec![drawer("a", "body")]);
    r.schema_version = 99;
    assert_eq!(evaluate(r).unwrap_err().code, "unsupported_version");
    assert_eq!(respond("not json")["error"]["code"], "invalid_json");
    let mut value = serde_json::to_value(request(Treatment::Raw, vec![])).unwrap();
    value["unknown"] = json!(true);
    assert_eq!(
        respond(&value.to_string())["error"]["code"],
        "invalid_request"
    );
}

#[tokio::test]
async fn production_backfill_skips_changed_text_when_id_is_present() {
    use trusty_memory::bm25_backfill::{backfill_palace, BackfillStatus, PalaceDocs};
    let temp = tempfile::tempdir().unwrap();
    let lane = trusty_memory::bm25_lane::Bm25Lane::with_limits(temp.path().into(), 1, None);
    let first = backfill_palace(
        &lane,
        "palace",
        PalaceDocs::from_pairs(vec![("same".into(), "oldterm".into())]),
        false,
    )
    .await;
    assert_eq!(first.status, BackfillStatus::Completed);
    let second = backfill_palace(
        &lane,
        "palace",
        PalaceDocs::from_pairs(vec![("same".into(), "newterm".into())]),
        false,
    )
    .await;
    assert_eq!(second.status, BackfillStatus::AlreadyIndexed);
    assert!(lane
        .search("palace", "newterm", 5)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(lane.search("palace", "oldterm", 5).await.unwrap().len(), 1);
}

#[test]
fn capacity_refusal_never_publishes_success() {
    const FLAG: &str = "TRUSTY_MEMORY_EVAL_CAPACITY_CHILD";
    if std::env::var_os(FLAG).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "support::tests::capacity_refusal_never_publishes_success",
                "--nocapture",
            ])
            .env(FLAG, "1")
            .env("TRUSTY_BM25_CORPUS_CAP", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
    } else {
        let r = request(
            Treatment::Raw,
            vec![drawer("a", "first"), drawer("b", "second")],
        );
        assert_eq!(evaluate(r).unwrap_err().code, "index_capacity");
    }
}

#[test]
fn aliases_do_not_cross_scope_and_corrupt_ranges_are_rejected() {
    let mut a = drawer("a", "first");
    a.aliases = vec!["same alias".into()];
    let mut b = drawer("b", "second");
    b.aliases = a.aliases.clone();
    let mut c = drawer("c", "third");
    c.aliases = a.aliases.clone();
    c.scope = "private".into();
    let mut r = request(Treatment::Kg, vec![a, b, c]);
    r.queries = vec![query("same alias")];
    let response = evaluate(r).unwrap();
    assert_eq!(
        response.results[0]
            .hits
            .iter()
            .map(|h| h.id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    let mut state = response.state;
    state.derived[0].byte_end = usize::MAX;
    assert_eq!(evaluate(resume(state)).unwrap_err().code, "invalid_state");
}

#[test]
fn canonical_metadata_and_dates_are_idempotent_retries() {
    let mut d = drawer("a", "body bytes unchanged");
    d.tags = vec!["b".into(), "a".into(), "a".into()];
    d.created_at = "2026-01-01T00:00:00.000Z".into();
    let first = evaluate(request(Treatment::Context, vec![d.clone()])).unwrap();
    d.tags = vec!["a".into(), "b".into()];
    d.created_at = "2026-01-01T00:00:00Z".into();
    let mut next = resume(first.state);
    next.mutations = vec![Mutation::Upsert {
        revision: 1,
        drawer: d.into(),
    }];
    let second = evaluate(next).unwrap();
    assert_eq!(second.maintenance.rewritten, 0);
}

#[test]
fn alias_candidates_survive_context_cap_without_links() {
    let mut a = drawer("a", "body");
    a.room = "aardvark".into();
    a.aliases = vec!["zebralocator".into()];
    let mut b = a.clone();
    b.id = "b".into();
    let mut r = request(Treatment::Kg, vec![a, b]);
    r.policy.context_tokens = 1;
    r.queries = vec![query("zebralocator")];
    let response = evaluate(r).unwrap();
    assert!(response
        .state
        .snapshot
        .iter()
        .all(|doc| !doc.text.contains("zebralocator")));
    assert_eq!(
        response.results[0]
            .hits
            .iter()
            .map(|h| h.id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert!(response.results[0]
        .hits
        .iter()
        .all(|h| h.bm25_score == 0.0 && h.origins == vec!["kg"]));
}

#[test]
fn extra_legacy_children_cannot_claim_a_fresh_generation() {
    let mut state = evaluate(request(
        Treatment::Context,
        vec![drawer("a", "source evidence")],
    ))
    .unwrap()
    .state;
    state.snapshot.push(Document {
        doc_id: doc_id("scope", "a", 9),
        text: "inventedneedle".into(),
    });
    let mut r = resume(state.clone());
    r.maintenance.max_documents = 0;
    r.queries = vec![query("inventedneedle")];
    assert_eq!(evaluate(r).unwrap_err().code, "invalid_state");
    state.derived.clear();
    let repaired = evaluate(resume(state)).unwrap();
    assert_eq!(repaired.state.snapshot.len(), 1);
    assert!(!repaired.state.snapshot[0].text.contains("inventedneedle"));
    assert_eq!(repaired.maintenance.fresh, 1);
}

#[test]
fn policy_bounds_match_the_frozen_interface() {
    assert_eq!(
        serde_json::from_value::<Policy>(json!({})).unwrap(),
        Policy::default()
    );
    assert_eq!(serde_json::from_value::<Policy>(json!({"context_tokens":null,"chunk_tokens":null,"freshness_weight":null,"kg_weight":null})).unwrap(),Policy::default());
    for policy in [
        Policy {
            context_tokens: 1,
            chunk_tokens: 32,
            freshness_weight: 0.0,
            kg_weight: 0.0,
        },
        Policy {
            context_tokens: 256,
            chunk_tokens: 1024,
            freshness_weight: 0.20,
            kg_weight: 0.30,
        },
    ] {
        let mut r = request(Treatment::Raw, vec![]);
        r.policy = policy;
        assert!(evaluate(r).is_ok());
    }
    for policy in [
        Policy {
            context_tokens: 0,
            ..Policy::default()
        },
        Policy {
            context_tokens: 257,
            ..Policy::default()
        },
        Policy {
            chunk_tokens: 31,
            ..Policy::default()
        },
        Policy {
            chunk_tokens: 1025,
            ..Policy::default()
        },
        Policy {
            freshness_weight: 0.201,
            ..Policy::default()
        },
        Policy {
            kg_weight: 0.301,
            ..Policy::default()
        },
    ] {
        let mut r = request(Treatment::Raw, vec![]);
        r.policy = policy;
        assert_eq!(evaluate(r).unwrap_err().code, "invalid_request");
    }
}

#[test]
fn repeated_occurrences_and_oversized_utf8_runs_are_bounded() {
    let repeated = std::iter::repeat_n("repeat", 669)
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(trusty_common::bm25::tokenize(&repeated).len(), 1);
    assert_eq!(derive::token_occurrences(&repeated), 669);
    let long_utf8 = "é".repeat(5000);
    let compound = "alpha1Beta ".repeat(200);
    let whitespace = " \n".repeat(4000);
    for body in [&repeated, &long_utf8, &compound, &whitespace] {
        let ranges = derive::ranges(body, 128, true);
        assert!(ranges.len() > 1);
        assert_eq!(ranges.first().unwrap().0, 0);
        assert_eq!(ranges.last().unwrap().1, body.len());
        assert!(ranges.windows(2).all(|pair| pair[0].1 == pair[1].0));
        assert_eq!(
            ranges
                .iter()
                .map(|(s, e)| &body[*s..*e])
                .collect::<String>(),
            *body
        );
        for (start, end) in ranges {
            assert!(start < end && body.is_char_boundary(start) && body.is_char_boundary(end));
            assert!(derive::token_occurrences(&body[start..end]) <= 128);
            assert!(end - start <= derive::MAX_CHUNK_BYTES);
        }
    }
    assert_eq!(derive::ranges(&repeated, 128, true).len(), 6);
    assert_eq!(derive::ranges(&repeated, 256, true).len(), 3);
    // Unsplit ablations remain controls; only the chunk treatment adds bounds.
    assert_eq!(
        derive::ranges(&long_utf8, 128, false),
        vec![(0, long_utf8.len())]
    );
}
