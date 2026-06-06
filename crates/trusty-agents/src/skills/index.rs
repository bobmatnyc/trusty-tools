//! BM25 semantic search index over available skills (#483).
//!
//! Why: Skills are currently injected statically at agent startup from
//! `[system_prompt] skills = [...]` in the TOML — every listed skill burns
//! context budget on every turn regardless of relevance. A lightweight BM25
//! index lets the harness rank ALL discoverable skills against the current
//! user message and inject only the top-N most relevant ones per turn, while
//! the statically-listed skills continue to be injected as before (the
//! dynamic search is purely additive).
//! What: `SkillIndex` scans a list of skill directories for `*.md` files,
//! reads an optional `<name>.yaml` sidecar for richer metadata (title,
//! description, tags), and builds an in-memory inverted BM25 index over the
//! concatenated `title + description + tags` text. `search` ranks documents
//! by BM25 score and returns up to N skill names (the `.md` file stem).
//! Test: See the `tests` module — `bm25_ranks_relevant_skill_higher`,
//! `search_returns_at_most_n_results`, `empty_query_returns_empty`,
//! `skill_without_yaml_uses_filename_as_title`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

/// BM25 term-frequency saturation parameter.
///
/// Why: Standard BM25 default; controls how quickly repeated terms stop
/// contributing additional weight.
const BM25_K1: f64 = 1.5;

/// BM25 document-length normalization parameter.
///
/// Why: Standard BM25 default; controls how strongly long documents are
/// penalized relative to the average document length.
const BM25_B: f64 = 0.75;

/// Optional `skill.yaml` sidecar metadata for one skill.
///
/// Why: The skill filename alone is a weak relevance signal. A sidecar lets
/// skill authors supply a human title, a one-line description, and tags that
/// all feed the BM25 document text — substantially improving ranking.
/// What: All fields are optional so a partial or absent sidecar degrades
/// gracefully. `always_inject` is parsed for completeness (callers may use it
/// to force a skill into the prompt) but does not affect BM25 scoring.
/// Test: `skill_without_yaml_uses_filename_as_title` covers the absent case.
#[derive(Debug, Default)]
struct SkillYaml {
    title: Option<String>,
    description: Option<String>,
    tags: Vec<String>,
    #[allow(dead_code)]
    always_inject: bool,
}

/// One indexed skill document.
///
/// Why: `search` needs per-document length and the resolvable skill name
/// without re-reading the filesystem; holding them in memory keeps ranking
/// allocation-light.
/// What: `name` is the `.md` file stem (the value `search` returns).
/// `term_count` is the total token count of the indexed text, used for BM25
/// length normalization. `always_inject` mirrors the sidecar flag.
/// Test: Indirect — exercised by every `tests` module case.
#[derive(Debug, Clone)]
struct SkillDoc {
    name: String,
    term_count: usize,
    #[allow(dead_code)]
    always_inject: bool,
}

/// In-memory BM25 index over all discoverable skills.
///
/// Why: Per-turn dynamic skill injection needs a ranker that works without an
/// embedding model or a new crate dependency. A classic BM25 inverted index
/// is ~60 lines of pure Rust, deterministic, and fast enough to run on every
/// chat turn.
/// What: `docs` holds one `SkillDoc` per indexed skill; `postings` is the
/// inverted index mapping each term to the `(doc_id, term_frequency)` pairs
/// it appears in; `avg_doc_len` is the mean document length used by BM25.
/// Test: `bm25_ranks_relevant_skill_higher` and the other `tests` cases.
#[derive(Debug, Default)]
pub struct SkillIndex {
    docs: Vec<SkillDoc>,
    /// Inverted index: term -> [(doc_id, term_frequency)].
    postings: HashMap<String, Vec<(usize, usize)>>,
    avg_doc_len: f64,
}

/// Split `text` into lowercase alphanumeric tokens.
///
/// Why: BM25 needs a consistent tokenization for both indexing and querying;
/// splitting on non-alphanumeric characters keeps punctuation, hyphens, and
/// markdown noise out of the term set.
/// What: Lowercases the input, splits on any character that is not
/// alphanumeric, and drops empty fragments.
/// Test: Indirect — every indexing/search test relies on this.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

impl SkillIndex {
    /// Build a BM25 index from all `*.md` files in the given directories.
    ///
    /// Why: The harness discovers skills across several priority-ordered
    /// directories (project-local, user-level, bundled). One index over all
    /// of them gives a single ranked view for per-turn injection.
    /// What: For each directory that exists, scans every `*.md` file. The skill
    /// name is the file stem. An optional `<stem>.yaml` sidecar is read for
    /// title/description/tags; absent or unparseable sidecars fall back to the
    /// stem as the title and empty description/tags. The indexed document text
    /// is `title + " " + description + " " + tags.join(" ")`. The first
    /// occurrence of a given skill name wins (later directories are
    /// lower-priority and skipped on conflict). Missing directories and
    /// unreadable files are silently skipped.
    /// Test: `bm25_ranks_relevant_skill_higher`,
    /// `skill_without_yaml_uses_filename_as_title`.
    pub fn build(skill_dirs: &[PathBuf]) -> Result<Self> {
        let mut docs: Vec<SkillDoc> = Vec::new();
        let mut postings: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        let mut seen: HashMap<String, ()> = HashMap::new();
        let mut total_len: usize = 0;

        for dir in skill_dirs {
            if !dir.is_dir() {
                continue;
            }
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(e) => {
                    tracing::debug!(dir = %dir.display(), error = %e, "skill index: read_dir failed");
                    continue;
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let name = stem.to_string();
                // First (highest-priority) occurrence wins.
                if seen.insert(name.clone(), ()).is_some() {
                    continue;
                }

                let meta = read_sidecar(&path);
                let title = meta
                    .title
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| name.clone());
                let description = meta.description.unwrap_or_default();
                let doc_text = format!("{title} {description} {}", meta.tags.join(" "));

                let tokens = tokenize(&doc_text);
                let term_count = tokens.len();
                total_len += term_count;

                let doc_id = docs.len();
                // Per-document term frequencies.
                let mut tf: HashMap<String, usize> = HashMap::new();
                for tok in tokens {
                    *tf.entry(tok).or_insert(0) += 1;
                }
                for (term, freq) in tf {
                    postings.entry(term).or_default().push((doc_id, freq));
                }

                docs.push(SkillDoc {
                    name,
                    term_count,
                    always_inject: meta.always_inject,
                });
            }
        }

        let avg_doc_len = if docs.is_empty() {
            0.0
        } else {
            total_len as f64 / docs.len() as f64
        };

        tracing::debug!(skills = docs.len(), "BM25 skill index built");
        Ok(Self {
            docs,
            postings,
            avg_doc_len,
        })
    }

    /// Number of indexed skills.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// True when the index has no skills.
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Return up to `n` skill names ranked by BM25 score against `query`.
    ///
    /// Why: Per-turn injection needs the few most relevant skills for the
    /// current user message; BM25 gives a principled relevance ordering
    /// without an embedding model.
    /// What: Tokenizes `query`, computes the BM25 score for every document
    /// that shares at least one query term, and returns the names of the
    /// top-`n` documents by descending score (ties broken by document id for
    /// determinism). Returns an empty vector when the query is blank, `n` is
    /// zero, or the index is empty.
    /// Test: `bm25_ranks_relevant_skill_higher`,
    /// `search_returns_at_most_n_results`, `empty_query_returns_empty`.
    pub fn search(&self, query: &str, n: usize) -> Vec<String> {
        if n == 0 || self.docs.is_empty() {
            return Vec::new();
        }
        let terms = tokenize(query);
        if terms.is_empty() {
            return Vec::new();
        }

        let total_docs = self.docs.len() as f64;
        // doc_id -> accumulated BM25 score.
        let mut scores: HashMap<usize, f64> = HashMap::new();

        for term in &terms {
            let Some(postings) = self.postings.get(term) else {
                continue;
            };
            let doc_freq = postings.len() as f64;
            // BM25 IDF with the standard +0.5 smoothing; max(.., 0) keeps the
            // weight non-negative for terms that appear in most documents.
            let idf = (((total_docs - doc_freq + 0.5) / (doc_freq + 0.5)) + 1.0).ln();
            for &(doc_id, tf) in postings {
                let doc_len = self.docs[doc_id].term_count as f64;
                let tf = tf as f64;
                let denom =
                    tf + BM25_K1 * (1.0 - BM25_B + BM25_B * (doc_len / self.avg_doc_len.max(1.0)));
                let weight = idf * (tf * (BM25_K1 + 1.0) / denom);
                *scores.entry(doc_id).or_insert(0.0) += weight;
            }
        }

        let mut ranked: Vec<(usize, f64)> = scores.into_iter().collect();
        // Sort by score descending; break ties by doc_id ascending so the
        // output is deterministic across runs.
        ranked.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        ranked
            .into_iter()
            .take(n)
            .map(|(doc_id, _)| self.docs[doc_id].name.clone())
            .collect()
    }
}

/// Read the optional `<stem>.yaml` sidecar next to a skill's `.md` file.
///
/// Why: Centralizes the best-effort sidecar load so `build` stays readable;
/// a missing or malformed sidecar must never abort indexing. The sidecar
/// format is dead-simple flat YAML (a handful of scalar keys plus one inline
/// list) so — matching the existing skill-frontmatter parser in this crate —
/// a hand-rolled parser avoids pulling in a full YAML crate dependency.
/// What: Derives the sidecar path by swapping the `.md` extension for `.yaml`,
/// returns `SkillYaml::default()` when the file is absent or unreadable, and
/// otherwise extracts `title`, `description`, `tags` (inline `[a, b]` list),
/// and `always_inject` (`true`/`false`).
/// Test: `skill_without_yaml_uses_filename_as_title` exercises the absent path;
/// `bm25_ranks_relevant_skill_higher` exercises a populated sidecar.
fn read_sidecar(md_path: &Path) -> SkillYaml {
    let yaml_path = md_path.with_extension("yaml");
    let Ok(text) = std::fs::read_to_string(&yaml_path) else {
        return SkillYaml::default();
    };
    SkillYaml {
        title: yaml_scalar(&text, "title"),
        description: yaml_scalar(&text, "description"),
        tags: yaml_inline_list(&text, "tags"),
        always_inject: yaml_scalar(&text, "always_inject")
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false),
    }
}

/// Extract a flat scalar value for `key` from minimal YAML text.
///
/// Why: `read_sidecar` needs `title`/`description`/`always_inject` without a
/// YAML crate; a line-scan keyed on `key:` matches the existing frontmatter
/// parser conventions in this crate.
/// What: Finds the first top-level line starting with `key:`, trims the value,
/// and strips surrounding single/double quotes. Returns `None` when absent or
/// the value is empty.
/// Test: Indirect via `read_sidecar` / `bm25_ranks_relevant_skill_higher`.
fn yaml_scalar(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            let val = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Extract an inline list value (`key: [a, b, c]`) from minimal YAML text.
///
/// Why: `tags` is the only list field in `skill.yaml`; supporting just the
/// inline form keeps the parser tiny while covering the documented format.
/// What: Finds the first line starting with `key:`, expects a `[...]` value,
/// splits on commas, and trims quotes/whitespace from each element. Returns an
/// empty vector when the key is absent or not an inline list.
/// Test: Indirect via `read_sidecar` / `bm25_ranks_relevant_skill_higher`.
fn yaml_inline_list(text: &str, key: &str) -> Vec<String> {
    let prefix = format!("{key}:");
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            let val = rest.trim();
            if let Some(inner) = val.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                return inner
                    .split(',')
                    .map(|t| t.trim().trim_matches('"').trim_matches('\'').to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "trusty-agents-skill-index-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn write_skill(dir: &Path, stem: &str, yaml: Option<&str>) {
        std::fs::write(dir.join(format!("{stem}.md")), "# body\n").unwrap();
        if let Some(y) = yaml {
            std::fs::write(dir.join(format!("{stem}.yaml")), y).unwrap();
        }
    }

    #[test]
    fn bm25_ranks_relevant_skill_higher() {
        let dir = tempdir();
        write_skill(
            &dir,
            "web-search",
            Some(
                "title: \"Web Search\"\ndescription: \"Search the web using Brave or DuckDuckGo\"\ntags: [\"search\", \"web\", \"research\"]\n",
            ),
        );
        write_skill(
            &dir,
            "rust-async",
            Some(
                "title: \"Rust Async\"\ndescription: \"Tokio runtime and async patterns\"\ntags: [\"rust\", \"async\", \"tokio\"]\n",
            ),
        );

        let index = SkillIndex::build(&[dir]).unwrap();
        let hits = index.search("how do I search the web", 5);
        assert!(!hits.is_empty(), "expected at least one hit");
        assert_eq!(
            hits[0], "web-search",
            "web-search should rank highest for a web-search query, got {hits:?}"
        );
    }

    #[test]
    fn search_returns_at_most_n_results() {
        let dir = tempdir();
        for i in 0..10 {
            write_skill(
                &dir,
                &format!("skill-{i}"),
                Some(&format!(
                    "title: \"Skill {i}\"\ndescription: \"common shared keyword indexing\"\ntags: [\"common\"]\n"
                )),
            );
        }
        let index = SkillIndex::build(&[dir]).unwrap();
        let hits = index.search("common keyword", 3);
        assert!(
            hits.len() <= 3,
            "search must cap at n=3, got {} results",
            hits.len()
        );
        assert_eq!(
            hits.len(),
            3,
            "expected exactly 3 results for a 10-doc index"
        );
    }

    #[test]
    fn empty_query_returns_empty() {
        let dir = tempdir();
        write_skill(
            &dir,
            "web-search",
            Some("title: \"Web Search\"\ndescription: \"search the web\"\ntags: [\"web\"]\n"),
        );
        let index = SkillIndex::build(&[dir]).unwrap();
        assert!(index.search("", 3).is_empty(), "blank query → empty");
        assert!(
            index.search("   ", 3).is_empty(),
            "whitespace-only query → empty"
        );
        assert!(
            index.search("!!! ??? ...", 3).is_empty(),
            "punctuation-only query → empty"
        );
    }

    #[test]
    fn empty_index_returns_empty() {
        let index = SkillIndex::build(&[]).unwrap();
        assert!(index.is_empty());
        assert!(index.search("anything", 3).is_empty());
    }

    #[test]
    fn skill_without_yaml_uses_filename_as_title() {
        let dir = tempdir();
        // No sidecar — the file stem must become the searchable title.
        write_skill(&dir, "database-migration", None);
        let index = SkillIndex::build(&[dir]).unwrap();
        assert_eq!(index.len(), 1);
        // The stem "database-migration" tokenizes to ["database", "migration"]
        // so a query on either token must surface the skill.
        let hits = index.search("database migration", 5);
        assert_eq!(
            hits,
            vec!["database-migration".to_string()],
            "filename stem should be indexed as the title when no yaml exists"
        );
    }

    #[test]
    fn higher_priority_dir_wins_on_name_conflict() {
        let high = tempdir();
        let low = tempdir();
        write_skill(
            &high,
            "shared",
            Some("title: \"High\"\ndescription: \"high priority alpha\"\ntags: []\n"),
        );
        write_skill(
            &low,
            "shared",
            Some("title: \"Low\"\ndescription: \"low priority beta\"\ntags: []\n"),
        );
        let index = SkillIndex::build(&[high, low]).unwrap();
        assert_eq!(index.len(), 1, "duplicate skill name must be deduplicated");
        // The high-priority doc indexed "alpha"; "beta" should not match.
        assert_eq!(index.search("alpha", 3), vec!["shared".to_string()]);
        assert!(
            index.search("beta", 3).is_empty(),
            "low-priority duplicate must be ignored"
        );
    }
}
