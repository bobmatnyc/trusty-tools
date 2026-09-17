//! Resident experimental BM25, native graph, and prompt formatter probe.
//!
//! Why: Measure query work independently from startup and index hydration.
//! What: JSONL requests address resident synthetic projections; no live stores.
//! Test: `format_and_protocol_are_explicit`, `resident_graph_preserves_direction`.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{self, BufRead, Write},
    path::PathBuf,
    time::Instant,
};
use trusty_common::memory_core::store::kg_store::is_functional_predicate;
use trusty_common::{
    bm25::BM25Index,
    memory_core::store::kg::{ExpandDirection, KnowledgeGraph, Triple},
};
use trusty_memory::prompt_facts::{build_prompt_context, is_hot_predicate};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    id: String,
    text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputTriple {
    id: String,
    subject: String,
    predicate: String,
    object: String,
    valid_from: chrono::DateTime<chrono::Utc>,
    valid_to: Option<chrono::DateTime<chrono::Utc>>,
}
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Load {
        projection: String,
        documents: Vec<Document>,
        triples: Vec<InputTriple>,
        scratch_dir: PathBuf,
    },
    Search {
        projection: String,
        text: String,
        limit: usize,
    },
    Format {
        triples: Vec<(String, String, String)>,
    },
    Classify {
        predicates: Vec<String>,
    },
    Graph {
        projection: String,
        method: String,
        entity: String,
        hops: usize,
    },
    CurrentPage {
        projection: String,
        limit: usize,
    },
    Close {},
}
struct Projection {
    index: BM25Index,
    graph: KnowledgeGraph,
    _directory: tempfile::TempDir,
}
#[derive(Default)]
struct Probe {
    projections: BTreeMap<String, Projection>,
}
impl Probe {
    /// Why: Keep measured operations resident and fail on malformed requests.
    /// What: Dispatch explicit operations; construction cost remains separate.
    /// Test: `resident_graph_preserves_direction`, `format_and_protocol_are_explicit`.
    async fn run(&mut self, operation: Operation) -> Result<Value> {
        match operation {
            Operation::Load {
                projection,
                mut documents,
                mut triples,
                scratch_dir,
            } => {
                if self.projections.contains_key(&projection) {
                    bail!("projection already loaded");
                }
                let started = Instant::now();
                documents.sort_by(|a, b| a.id.cmp(&b.id));
                triples.sort_by(|a, b| a.id.cmp(&b.id));
                let mut index = BM25Index::new();
                for document in &documents {
                    index.upsert_document(&document.id, &document.text);
                }
                let bm25_ns = started.elapsed().as_nanos();
                let directory = tempfile::Builder::new()
                    .prefix("memory-prompt-probe-")
                    .tempdir_in(scratch_dir)?;
                let path = directory.path().join("synthetic.db");
                let graph_start = Instant::now();
                let graph = KnowledgeGraph::open(&path)?;
                let imported = triples
                    .iter()
                    .map(|item| Triple {
                        subject: item.subject.clone(),
                        predicate: item.predicate.clone(),
                        object: item.object.clone(),
                        valid_from: item.valid_from,
                        valid_to: item.valid_to,
                        confidence: 1.0,
                        provenance: Some(item.id.clone()),
                    })
                    .collect();
                graph.store().import_all(imported, Vec::new())?;
                let graph_build_ns = graph_start.elapsed().as_nanos();
                drop(graph);
                let hydrate = Instant::now();
                let graph = KnowledgeGraph::open(&path)?;
                let hydration_ns = hydrate.elapsed().as_nanos();
                let stored = graph.count_active_triples()?;
                let active = graph.list_active(usize::MAX, 0).await?;
                let expected: std::collections::BTreeSet<_> = active
                    .iter()
                    .map(|t| {
                        (
                            &t.subject,
                            &t.predicate,
                            if is_functional_predicate(&t.predicate) {
                                ""
                            } else {
                                t.object.as_str()
                            },
                        )
                    })
                    .collect();
                if graph.edge_count() != expected.len() {
                    bail!("hydrated adjacency differs from native functional-predicate rules");
                }
                for item in triples.iter().filter(|t| t.valid_to.is_none()).take(1) {
                    if graph
                        .expand_neighbors(&item.subject, ExpandDirection::Both, 1)?
                        .1
                        .is_empty()
                    {
                        bail!("known seeded graph edge disappeared during hydration");
                    }
                }
                let mut disk_bytes = 0;
                for entry in std::fs::read_dir(directory.path())? {
                    disk_bytes += entry?.metadata()?.len();
                }
                let result = json!({"documents":index.len(),"input_triples":triples.len(),"edges":graph.edge_count(),"active_store_rows":stored,
                    "bm25_build_ns":bm25_ns,"graph_build_ns":graph_build_ns,"hydration_ns":hydration_ns,"disk_bytes":disk_bytes});
                self.projections.insert(
                    projection,
                    Projection {
                        index,
                        graph,
                        _directory: directory,
                    },
                );
                Ok(result)
            }
            Operation::Search {
                projection,
                text,
                limit,
            } => {
                let p = self
                    .projections
                    .get(&projection)
                    .context("unknown projection")?;
                // Score the full resident candidate set before deterministic tie truncation.
                let mut hits = p.index.score_query_all(&text, p.index.len());
                hits.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
                hits.truncate(limit);
                Ok(
                    json!({"hits":hits.into_iter().map(|(id,score)| json!({"id":id,"score":score})).collect::<Vec<_>>()}),
                )
            }
            Operation::Format { triples } => Ok(json!({"text":build_prompt_context(&triples)})),
            Operation::Classify { predicates } => Ok(
                json!({"hot":predicates.iter().map(|p| is_hot_predicate(p)).collect::<Vec<_>>()}),
            ),
            Operation::Graph {
                projection,
                method,
                entity,
                hops,
            } => {
                if !(1..=2).contains(&hops) {
                    bail!("hops must be 1 or 2");
                }
                let graph = &self
                    .projections
                    .get(&projection)
                    .context("unknown projection")?
                    .graph;
                let start = Instant::now();
                let mut triples = match method.as_str() {
                    "query_active" => graph.query_active(&entity).await?,
                    "expand_neighbors" => {
                        graph
                            .expand_neighbors(&entity, ExpandDirection::Both, hops)?
                            .1
                    }
                    _ => bail!("unknown graph method"),
                };
                let api_ns = start.elapsed().as_nanos();
                triples.sort_by(|a, b| {
                    (&a.subject, &a.predicate, &a.object).cmp(&(
                        &b.subject,
                        &b.predicate,
                        &b.object,
                    ))
                });
                Ok(json!({"api_ns":api_ns,"edges":triples.len(),"triples":triples}))
            }
            Operation::CurrentPage { projection, limit } => {
                if limit != 200 {
                    bail!("current page limit must be 200");
                }
                let graph = &self
                    .projections
                    .get(&projection)
                    .context("unknown projection")?
                    .graph;
                let start = Instant::now();
                let triples = graph.list_active(limit, 0).await?;
                Ok(json!({"api_ns":start.elapsed().as_nanos(),"triples":triples}))
            }
            Operation::Close {} => Ok(json!({"closed":true})),
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut probe = Probe::default();
    for line in io::stdin().lock().lines() {
        let start = Instant::now();
        let line = line?;
        let mut id = Value::Null;
        let parsed = (|| -> Result<Operation> {
            let mut value: Value = serde_json::from_str(&line)?;
            let object = value.as_object_mut().context("request must be an object")?;
            id = object.remove("id").context("missing request id")?;
            if id.as_str().is_none_or(str::is_empty) {
                bail!("request id must be a nonempty string");
            }
            Ok(serde_json::from_value(value)?)
        })();
        let close = matches!(&parsed, Ok(Operation::Close {}));
        let result = match parsed {
            Ok(op) => probe.run(op).await,
            Err(error) => Err(error),
        };
        let reply = match result {
            Ok(result) => {
                json!({"id":id,"ok":true,"result":result,"elapsed_ns":start.elapsed().as_nanos()})
            }
            Err(error) => {
                json!({"id":id,"ok":false,"error":{"code":"protocol_error","message":error.to_string()}})
            }
        };
        println!("{}", serde_json::to_string(&reply)?);
        io::stdout().flush()?;
        if close {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn format_and_protocol_are_explicit() -> Result<()> {
        let mut probe = Probe::default();
        let value = probe
            .run(Operation::Format {
                triples: vec![("s".into(), "is_fact".into(), "Complete claim.".into())],
            })
            .await?;
        assert!(
            value["text"]
                .as_str()
                .context("text")?
                .contains("Complete claim.")
        );
        assert!(serde_json::from_str::<Operation>(r#"{"op":"close","unknown":1}"#).is_err());
        Ok(())
    }
    #[tokio::test]
    async fn resident_graph_preserves_direction() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut probe = Probe::default();
        probe
            .run(Operation::Load {
                projection: "a".into(),
                documents: vec![Document {
                    id: "d".into(),
                    text: "alpha link".into(),
                }],
                triples: vec![InputTriple {
                    id: "t".into(),
                    subject: "alpha".into(),
                    predicate: "depends_on".into(),
                    object: "beta".into(),
                    valid_from: "2026-01-01T00:00:00Z".parse()?,
                    valid_to: None,
                }],
                scratch_dir: directory.path().into(),
            })
            .await?;
        for method in ["query_active", "expand_neighbors"] {
            let value = probe
                .run(Operation::Graph {
                    projection: "a".into(),
                    method: method.into(),
                    entity: "alpha".into(),
                    hops: 1,
                })
                .await?;
            assert_eq!(value["triples"][0]["subject"], "alpha");
            assert_eq!(value["triples"][0]["object"], "beta");
        }
        let value = probe
            .run(Operation::Search {
                projection: "a".into(),
                text: "alpha".into(),
                limit: 20,
            })
            .await?;
        assert_eq!(value["hits"][0]["id"], "d");
        Ok(())
    }
}
