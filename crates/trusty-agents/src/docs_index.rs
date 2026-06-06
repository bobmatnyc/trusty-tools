//! Lightweight TF-IDF document index for searching project documentation.
//!
//! Why: CTRL needs to answer questions about how trusty-agents works (configuration,
//! agents, skills, workflows) without round-tripping every query to an LLM or
//! depending on a heavyweight vector database. A small TF-IDF index built once
//! at startup keeps the dependency surface flat (no model downloads, no extra
//! crates) and is plenty for hundreds of Markdown docs.
//! What: `DocsIndex::build` walks a docs directory, parses `*.md` files, and
//! builds an in-memory TF-IDF representation. `DocsIndex::search` returns the
//! top-N entries by cosine similarity against the query's TF-IDF vector.
//! Test: see `tests` module — `docs_index_finds_relevant_document`,
//! `docs_index_returns_empty_on_no_match`, `docs_index_top_n_limit`.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use walkdir::WalkDir;

/// One indexed document.
#[derive(Debug, Clone)]
pub struct DocEntry {
    /// Relative path from the index root (e.g. `user/quickstart.md`).
    pub path: String,
    /// First `# heading` in the document, falling back to the file stem.
    pub title: String,
    /// Full text content (used for snippet generation).
    #[allow(dead_code)]
    pub content: String,
    /// First 300 characters for preview.
    pub snippet: String,
    /// L2-normalized TF-IDF vector keyed by term.
    tfidf: HashMap<String, f32>,
}

/// One search hit.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub path: String,
    pub title: String,
    pub snippet: String,
    pub score: f32,
}

/// In-memory TF-IDF index over a corpus of Markdown documents.
pub struct DocsIndex {
    documents: Vec<DocEntry>,
    /// Inverse document frequency per term: ln((N+1) / (df+1)) + 1.
    idf: HashMap<String, f32>,
}

impl DocsIndex {
    /// Why: Used as the empty fallback when the docs directory is absent.
    /// What: Returns an index with zero documents; `search` always returns
    /// `vec![]`.
    /// Test: indirectly via `docs_index_returns_empty_on_no_match`.
    pub fn empty() -> Self {
        Self {
            documents: Vec::new(),
            idf: HashMap::new(),
        }
    }

    /// Number of indexed documents.
    pub fn len(&self) -> usize {
        self.documents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }

    /// Build an index from all `*.md` files under `docs_dir` (recursive).
    ///
    /// Why: Project docs live as Markdown in `docs/`; we want one entry per
    /// file, indexed at startup, so search latency is purely arithmetic.
    /// What: Walks the directory, reads each `.md` file, tokenizes, computes
    /// TF, then derives IDF across the corpus and L2-normalized TF-IDF
    /// vectors. Files that fail to read are skipped (logged via tracing).
    /// Test: `docs_index_finds_relevant_document` builds an index from a temp
    /// directory and asserts the right document ranks first.
    pub fn build(docs_dir: &Path) -> Self {
        if !docs_dir.exists() {
            return Self::empty();
        }
        let root = docs_dir.to_path_buf();

        // 1. Collect raw documents (path, content, term-frequencies).
        struct Raw {
            path: String,
            title: String,
            content: String,
            snippet: String,
            tf: HashMap<String, f32>,
        }
        let mut raws: Vec<Raw> = Vec::new();

        for entry in WalkDir::new(&root)
            .into_iter()
            .filter_entry(|e| {
                // Prune denied directories (.git, node_modules, target, …)
                // before descending — saves a lot of walk time on big trees.
                if e.file_type().is_dir()
                    && let Some(name) = e.file_name().to_str()
                {
                    return !crate::tools::file_filter::should_skip_dir(name);
                }
                true
            })
            .filter_map(std::result::Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            // Docs index is markdown-only; the shared filter still rejects
            // oversize files and anything we surfaced inside a skip-dir.
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            if crate::tools::file_filter::should_skip_file(path) {
                continue;
            }
            let rel = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            let content = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "docs_index: read failed");
                    continue;
                }
            };
            let title = extract_title(&content).unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| rel.clone())
            });
            let snippet = build_snippet(&content);
            let tf = compute_tf(&content);
            raws.push(Raw {
                path: rel,
                title,
                content,
                snippet,
                tf,
            });
        }

        // 2. Document frequency per term.
        let n = raws.len();
        let mut df: HashMap<String, usize> = HashMap::new();
        for r in &raws {
            for term in r.tf.keys() {
                *df.entry(term.clone()).or_insert(0) += 1;
            }
        }
        // 3. IDF: ln((N+1)/(df+1)) + 1 (smoothed; never zero).
        let mut idf: HashMap<String, f32> = HashMap::with_capacity(df.len());
        let n_f = (n as f32) + 1.0;
        for (term, count) in df {
            let v = ((n_f) / ((count as f32) + 1.0)).ln() + 1.0;
            idf.insert(term, v);
        }

        // 4. Per-doc TF-IDF vectors, L2-normalized.
        let mut documents: Vec<DocEntry> = Vec::with_capacity(raws.len());
        for r in raws {
            let mut vec: HashMap<String, f32> = HashMap::with_capacity(r.tf.len());
            for (term, tf) in &r.tf {
                let w = idf.get(term).copied().unwrap_or(1.0);
                vec.insert(term.clone(), tf * w);
            }
            l2_normalize(&mut vec);
            documents.push(DocEntry {
                path: r.path,
                title: r.title,
                content: r.content,
                snippet: r.snippet,
                tfidf: vec,
            });
        }

        Self { documents, idf }
    }

    /// Search the corpus, returning the top-N matches by cosine similarity.
    ///
    /// Why: The whole point of the index — let CTRL (and the API) answer
    /// "where is this concept documented?" without an LLM call.
    /// What: Tokenizes the query, builds an L2-normalized TF-IDF vector
    /// using the corpus's IDF, scores each document by dot product (cosine
    /// since vectors are unit-normalized), and returns the top `top_n` by
    /// descending score. Documents with score `<= 0` are filtered.
    /// Test: `docs_index_finds_relevant_document` and `docs_index_top_n_limit`.
    pub fn search(&self, query: &str, top_n: usize) -> Vec<SearchResult> {
        if self.documents.is_empty() || top_n == 0 {
            return Vec::new();
        }
        let q_tf = compute_tf(query);
        if q_tf.is_empty() {
            return Vec::new();
        }
        let mut q_vec: HashMap<String, f32> = HashMap::with_capacity(q_tf.len());
        for (term, tf) in &q_tf {
            // Unknown terms get a default IDF of 1.0 so partial overlap still
            // contributes; the corpus IDF dominates when present.
            let w = self.idf.get(term).copied().unwrap_or(1.0);
            q_vec.insert(term.clone(), tf * w);
        }
        l2_normalize(&mut q_vec);
        if q_vec.is_empty() {
            return Vec::new();
        }

        let mut hits: Vec<(usize, f32)> = self
            .documents
            .iter()
            .enumerate()
            .map(|(i, d)| (i, dot(&q_vec, &d.tfidf)))
            .filter(|(_, s)| *s > 0.0)
            .collect();
        hits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(top_n);

        hits.into_iter()
            .map(|(i, score)| {
                let d = &self.documents[i];
                SearchResult {
                    path: d.path.clone(),
                    title: d.title.clone(),
                    snippet: d.snippet.clone(),
                    score,
                }
            })
            .collect()
    }
}

/// Tokenize on non-alphanumeric boundaries, lowercase, drop short tokens
/// and stop-words. Also strips numbers (token must contain at least one
/// alphabetic char).
///
/// Why: Cheap, language-agnostic enough for English Markdown; nothing
/// fancy is needed for ~hundreds of docs.
fn tokenize(text: &str) -> Vec<String> {
    let stop: std::collections::HashSet<&'static str> = [
        "a", "an", "the", "and", "or", "but", "if", "is", "are", "was", "were", "be", "been",
        "being", "to", "of", "in", "on", "for", "with", "as", "at", "by", "from", "this", "that",
        "these", "those", "it", "its", "do", "does", "did", "have", "has", "had", "you", "your",
        "we", "our", "they", "their", "i", "me", "my", "he", "she", "his", "her", "not", "no",
        "so", "than", "then", "there", "here", "what", "which", "who", "whom", "how", "when",
        "where", "why", "can", "will", "would", "should", "could", "may", "might", "must", "into",
        "about", "over", "under", "via", "per", "within", "use", "used", "uses", "using",
    ]
    .into_iter()
    .collect();

    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter_map(|w| {
            if w.len() < 2 {
                return None;
            }
            let lower = w.to_lowercase();
            if !lower.chars().any(|c| c.is_alphabetic()) {
                return None;
            }
            if stop.contains(lower.as_str()) {
                return None;
            }
            Some(lower)
        })
        .collect()
}

/// Compute term frequencies normalized by total token count.
fn compute_tf(text: &str) -> HashMap<String, f32> {
    let tokens = tokenize(text);
    let total = tokens.len();
    if total == 0 {
        return HashMap::new();
    }
    let mut counts: HashMap<String, f32> = HashMap::new();
    for t in tokens {
        *counts.entry(t).or_insert(0.0) += 1.0;
    }
    let denom = total as f32;
    for v in counts.values_mut() {
        *v /= denom;
    }
    counts
}

/// L2-normalize a sparse vector in-place.
fn l2_normalize(v: &mut HashMap<String, f32>) {
    let norm: f32 = v.values().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.values_mut() {
            *x /= norm;
        }
    }
}

/// Sparse dot product. Iterates over the smaller map.
fn dot(a: &HashMap<String, f32>, b: &HashMap<String, f32>) -> f32 {
    let (small, big) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut s = 0.0;
    for (k, v) in small {
        if let Some(other) = big.get(k) {
            s += v * other;
        }
    }
    s
}

/// Pull the first level-1 heading from a Markdown document, if any.
fn extract_title(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# ") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// First 300 chars of body text (skipping the title line if any).
fn build_snippet(content: &str) -> String {
    let body: String = content
        .lines()
        .filter(|l| !l.trim().starts_with('#'))
        .collect::<Vec<_>>()
        .join(" ");
    let collapsed: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = collapsed.chars().take(300).collect();
    if collapsed.chars().count() > 300 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn docs_index_finds_relevant_document() {
        let dir = std::env::temp_dir().join(format!("docs_index_test_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("agents.md"),
            "# Agents\n\ntrusty-agents agents are defined in TOML files. Each agent has a model and system prompt.\n",
        )
        .unwrap();
        fs::write(
            dir.join("workflows.md"),
            "# Workflows\n\nWorkflows are JSON pipelines that orchestrate phases like research, plan, code, qa.\n",
        )
        .unwrap();
        fs::write(
            dir.join("install.md"),
            "# Install\n\nClone the repository and run cargo build to compile the binary.\n",
        )
        .unwrap();

        let idx = DocsIndex::build(&dir);
        assert!(idx.len() == 3, "expected 3 docs, got {}", idx.len());

        let hits = idx.search("how do agents work and what is the agent TOML", 3);
        assert!(!hits.is_empty(), "expected at least one hit");
        assert_eq!(hits[0].path, "agents.md");
        assert!(hits[0].score > 0.0);
        assert_eq!(hits[0].title, "Agents");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn docs_index_returns_empty_on_no_match() {
        let dir = std::env::temp_dir().join(format!("docs_index_empty_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("only.md"),
            "# Only\n\nrust async tokio mpsc channel.\n",
        )
        .unwrap();

        let idx = DocsIndex::build(&dir);
        let hits = idx.search("zzzzz qqqqq", 5);
        assert!(hits.is_empty(), "expected no hits, got {:?}", hits);

        // Also verify a totally empty index returns empty.
        let empty = DocsIndex::empty();
        assert!(empty.is_empty());
        assert!(empty.search("anything", 3).is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn docs_index_top_n_limit() {
        let dir = std::env::temp_dir().join(format!("docs_index_topn_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        for i in 0..6 {
            fs::write(
                dir.join(format!("doc{}.md", i)),
                format!("# Doc {i}\n\nrust tokio async runtime tutorial example {i}\n"),
            )
            .unwrap();
        }

        let idx = DocsIndex::build(&dir);
        assert_eq!(idx.len(), 6);

        let hits = idx.search("rust tokio async runtime", 3);
        assert_eq!(hits.len(), 3, "expected exactly 3 hits, got {}", hits.len());
        // Scores should be in descending order.
        for w in hits.windows(2) {
            assert!(w[0].score >= w[1].score);
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
