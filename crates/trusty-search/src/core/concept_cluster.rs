//! ConceptCluster entity generation via k-means clustering of doc comment embeddings.
//!
//! Why: doc comments express intent in natural language; clustering their
//! embeddings surfaces themes (auth, caching, parsing, …) that exact-string
//! search and even per-chunk vector search miss. Each cluster becomes one
//! `ConceptCluster` entity that participates in KG expansion and BM25
//! virtual-term enrichment.
//!
//! What: extract `///` doc lines from each chunk's content, embed every
//! non-empty doc string, run k-means (k = `min(20, n/2)`), then label each
//! cluster by finding the [`CONCEPT_VOCAB`] word whose embedding is nearest
//! the centroid (cosine). The cluster anchors to the chunk whose embedding
//! is closest to the centroid.
//!
//! Test: see `#[cfg(test)]` — sparse-doc inputs short-circuit with empty
//! output; vocab is non-empty. Live fastembed paths are skipped at unit-test
//! time (no `#[ignore]`-d slow tests live here; the fastembed model load is
//! exercised in `embed::tests`).

use ndarray::{Array1, Array2};

use crate::core::entity::{EntityType, RawEntity};

/// Vocabulary seed for cluster labeling (nearest-centroid wins).
///
/// Kept short — every word here is embedded *per file* at cluster time. These
/// are the umbrella themes most commonly surfaced in source-code doc comments
/// across systems work; tune as the corpus reveals new dominant clusters.
const CONCEPT_VOCAB: &[&str] = &[
    "authentication",
    "serialization",
    "caching",
    "networking",
    "persistence",
    "testing",
    "error-handling",
    "indexing",
    "parsing",
    "concurrency",
    "embedding",
    "search",
    "compression",
    "validation",
    "graph",
];

/// Minimum number of doc-comment strings required before clustering runs.
/// Below this we don't have enough signal to form meaningful clusters.
const MIN_DOC_STRINGS: usize = 4;

/// Cap on cluster count. Most files concentrate around a handful of themes.
const MAX_CLUSTERS: usize = 20;

/// Extract `///` doc-comment text from a chunk's content. Returns the
/// concatenation of all `///`-prefixed lines (stripped of the prefix and any
/// surrounding whitespace), or `None` if the chunk has no doc comments.
fn doc_comment(content: &str) -> Option<String> {
    let mut lines: Vec<&str> = Vec::new();
    for raw in content.lines() {
        let trimmed = raw.trim_start();
        if let Some(rest) = trimmed.strip_prefix("///") {
            lines.push(rest.trim());
        }
    }
    if lines.is_empty() {
        None
    } else {
        let joined = lines.join(" ");
        if joined.trim().is_empty() {
            None
        } else {
            Some(joined)
        }
    }
}

use crate::core::mmr::cosine_similarity as cosine;

/// Slugify a label for use in stable entity ids: lowercase ASCII, non-alnum → `-`.
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Run k-means on `embeddings` (each row a vector). Returns
/// `(centroids, assignments)` where `assignments[i]` is the cluster index of
/// row `i`. Returns `None` if linfa rejects the input (e.g. k > n) or the
/// underlying call panics.
fn kmeans_cluster(embeddings: &[Vec<f32>], k: usize) -> Option<(Vec<Vec<f32>>, Vec<usize>)> {
    use linfa::prelude::*;
    use linfa_clustering::KMeans;

    let n = embeddings.len();
    if n == 0 || k == 0 || k > n {
        return None;
    }
    let dim = embeddings[0].len();
    if dim == 0 {
        return None;
    }

    // Build the (n, dim) f64 matrix linfa expects.
    let flat: Vec<f64> = embeddings
        .iter()
        .flat_map(|v| v.iter().map(|&x| x as f64))
        .collect();
    let arr = Array2::from_shape_vec((n, dim), flat).ok()?;
    let dataset = DatasetBase::from(arr);

    // linfa::KMeans can panic on degenerate configs; isolate it.
    let model = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        KMeans::params(k)
            .max_n_iterations(50)
            .tolerance(1e-4)
            .fit(&dataset)
    }))
    .ok()?
    .ok()?;

    let assignments_arr: Array1<usize> = model.predict(dataset.records());
    let centroids_arr = model.centroids().clone();

    let centroids: Vec<Vec<f32>> = centroids_arr
        .outer_iter()
        .map(|row| row.iter().map(|&x| x as f32).collect())
        .collect();
    let assignments: Vec<usize> = assignments_arr.to_vec();

    Some((centroids, assignments))
}

/// Cluster doc comment embeddings from all chunks in a file and emit
/// ConceptCluster entities.
///
/// Steps:
/// 1. Extract doc comment text (lines starting with `///`) from each chunk
///    content.
/// 2. Embed each non-empty doc string using the provided embedder.
/// 3. K-means cluster (`k = min(MAX_CLUSTERS, n/2)`, at least 1) using
///    linfa-clustering.
/// 4. Label each cluster by finding the [`CONCEPT_VOCAB`] word whose
///    embedding is nearest the centroid (cosine).
/// 5. Emit one [`RawEntity`] per cluster whose `text` is the label and whose
///    span is `(0, 0)` — the entity is a file-level concept, not a span.
///
/// Returns an empty vec if fewer than [`MIN_DOC_STRINGS`] doc strings are
/// found (insufficient signal to cluster).
pub async fn cluster_concepts(
    chunks: &[crate::core::indexer::CodeChunk],
    embedder: &crate::core::embed::FastEmbedder,
    file: &str,
) -> Vec<RawEntity> {
    let contents: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
    cluster_concepts_from_contents(&contents, embedder, file).await
}

/// As [`cluster_concepts`] but accepts raw `&str` content slices, so callers
/// holding [`crate::core::chunker::RawChunk`] (the indexing path) don't need to
/// materialise full [`crate::core::indexer::CodeChunk`] values just to cluster.
pub async fn cluster_concepts_from_contents<E: crate::core::embed::Embedder + ?Sized>(
    contents: &[&str],
    embedder: &E,
    file: &str,
) -> Vec<RawEntity> {
    // 1) Extract doc strings.
    let docs: Vec<String> = contents.iter().filter_map(|c| doc_comment(c)).collect();
    if docs.len() < MIN_DOC_STRINGS {
        return Vec::new();
    }

    // 2) Embed each doc string. Bail (empty) if embedding fails — clustering
    //    is opt-in enrichment, never block indexing.
    let doc_refs: Vec<&str> = docs.iter().map(|s| s.as_str()).collect();
    let doc_embeddings = match embedder.embed_batch(&doc_refs).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("cluster_concepts: doc embedding failed for {file}: {e:#}");
            return Vec::new();
        }
    };
    if doc_embeddings.len() != docs.len() || doc_embeddings.is_empty() {
        return Vec::new();
    }

    // 3) K-means.
    let k = (docs.len() / 2).clamp(1, MAX_CLUSTERS);
    let Some((centroids, _assignments)) = kmeans_cluster(&doc_embeddings, k) else {
        tracing::debug!(
            "cluster_concepts: kmeans rejected k={k} n={} for {file}",
            docs.len()
        );
        return Vec::new();
    };

    // 4) Embed vocab once for labeling.
    let vocab_refs: Vec<&str> = CONCEPT_VOCAB.to_vec();
    let vocab_embeddings = match embedder.embed_batch(&vocab_refs).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("cluster_concepts: vocab embedding failed for {file}: {e:#}");
            return Vec::new();
        }
    };
    if vocab_embeddings.len() != CONCEPT_VOCAB.len() {
        return Vec::new();
    }

    // 5) Label each centroid with its nearest vocab word; emit one entity per cluster.
    let mut out: Vec<RawEntity> = Vec::with_capacity(centroids.len());
    let mut seen_labels: std::collections::HashSet<String> = std::collections::HashSet::new();
    for centroid in &centroids {
        let mut best_idx = 0usize;
        let mut best_sim = f32::NEG_INFINITY;
        for (i, vv) in vocab_embeddings.iter().enumerate() {
            let s = cosine(centroid, vv);
            if s > best_sim {
                best_sim = s;
                best_idx = i;
            }
        }
        let label = CONCEPT_VOCAB[best_idx];
        // Dedupe: if two centroids snap to the same vocab word, only emit once.
        if !seen_labels.insert(label.to_string()) {
            continue;
        }
        let slug = slugify(label);
        let id = format!("{file}:cluster:{slug}");
        out.push(RawEntity {
            id,
            entity_type: EntityType::ConceptCluster,
            text: label.to_string(),
            span: (0, 0),
            file: file.to_string(),
            line: 0,
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::chunker::ChunkType;
    use crate::core::embed::MockEmbedder;
    use crate::core::indexer::CodeChunk;

    fn chunk_with(content: &str) -> CodeChunk {
        CodeChunk {
            id: "x".into(),
            file: "x.rs".into(),
            path: None,
            language: None,
            start_line: 1,
            end_line: 1 + content.lines().count(),
            content: content.into(),
            function_name: None,
            score: 0.0,
            compact_snippet: None,
            match_reason: "test".into(),
            chunk_type: ChunkType::Code,
            calls: vec![],
            inherits_from: vec![],
            chunk_depth: 0,
            index_id: None,
        }
    }

    #[test]
    fn concept_vocab_is_nonempty() {
        assert!(!CONCEPT_VOCAB.is_empty());
    }

    #[test]
    fn doc_comment_extracts_triple_slash_lines() {
        let c = "/// hello world\n/// second line\nfn f() {}\n";
        assert_eq!(doc_comment(c).as_deref(), Some("hello world second line"));
    }

    #[test]
    fn doc_comment_returns_none_when_no_docs() {
        assert!(doc_comment("fn f() {}\n// regular comment\n").is_none());
    }

    #[test]
    fn slugify_lowercases_and_dashes() {
        assert_eq!(slugify("Error Handling"), "error-handling");
        assert_eq!(slugify("error-handling"), "error-handling");
    }

    fn contents_of(chunks: &[CodeChunk]) -> Vec<&str> {
        chunks.iter().map(|c| c.content.as_str()).collect()
    }

    #[tokio::test]
    async fn concept_cluster_empty_on_sparse_docs() {
        // Three chunks, none with doc comments → returns empty vec
        // (even before reaching the embedder).
        let chunks = vec![
            chunk_with("fn a() {}"),
            chunk_with("fn b() {}"),
            chunk_with("fn c() {}"),
        ];
        let embedder = MockEmbedder::new(8);
        let out = cluster_concepts_from_contents(&contents_of(&chunks), &embedder, "f.rs").await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn concept_cluster_empty_on_three_doc_strings() {
        // Below MIN_DOC_STRINGS (4) → empty.
        let chunks = vec![
            chunk_with("/// alpha\nfn a() {}"),
            chunk_with("/// beta\nfn b() {}"),
            chunk_with("/// gamma\nfn c() {}"),
        ];
        let embedder = MockEmbedder::new(8);
        let out = cluster_concepts_from_contents(&contents_of(&chunks), &embedder, "f.rs").await;
        assert!(out.is_empty(), "got {out:?}");
    }

    #[tokio::test]
    async fn concept_cluster_emits_entities_when_enough_docs() {
        // Four+ doc strings → clustering runs against MockEmbedder. We don't
        // assert specific labels (vocab pick depends on the deterministic
        // mock's hashing), but we do require ConceptCluster entities to come
        // out anchored to the file.
        let chunks = vec![
            chunk_with("/// authentication helper\nfn a() {}"),
            chunk_with("/// caches request results\nfn b() {}"),
            chunk_with("/// parses incoming bytes\nfn c() {}"),
            chunk_with("/// validates user input\nfn d() {}"),
            chunk_with("/// serializes the response\nfn e() {}"),
        ];
        let embedder = MockEmbedder::new(16);
        let out = cluster_concepts_from_contents(&contents_of(&chunks), &embedder, "svc.rs").await;
        // K-means with the deterministic MockEmbedder may collapse all rows
        // into one cluster — that's fine; we just need at least one entity
        // and the right type / file.
        assert!(!out.is_empty(), "expected at least one cluster entity");
        for ent in &out {
            assert_eq!(ent.entity_type, EntityType::ConceptCluster);
            assert_eq!(ent.file, "svc.rs");
            assert_eq!(ent.span, (0, 0));
            assert!(ent.id.starts_with("svc.rs:cluster:"));
        }
    }
}
