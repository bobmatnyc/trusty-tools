//! Search-driven evidence discovery: which files carry evidence for which DD
//! dimension, asked of the repository's own trusty-search index (#6082).
//!
//! Why: selection used to rank a repository by PATH NAME and by measured
//! complexity, and both are blind to the question the report actually asks. A
//! path name knows nothing about what a file does, and complexity is one
//! dimension pretending to be six — the last sweep's complexity-driven ranking
//! examined 20 files covering 2 of the 6 DD dimensions, where the path-name
//! ranking it replaced reached 5. Neither reads the code. trusty-search has
//! already read it, chunk by chunk, and #6081 indexes every audited repository
//! before this runs, so the index can be ASKED "where is the credential
//! handling" instead of guessed at.
//!
//! What: one query set per DD dimension (plus queries derived from the
//! analyst's own instructions), each run against `POST
//! /indexes/{id}/search`, each hit collapsed to a repo-relative file that
//! remembers WHICH query found it. [`blend`] then interleaves the dimensions
//! round-robin with the complexity ranking, so the budget is spent across the
//! dimensions rather than down one of them.
//!
//! The dimension names are trusty-review's, spelled identically
//! (`report::investigate::select::DIMENSIONS`). That is what lets the rendered
//! coverage section attribute an examined file to a dimension without either
//! crate translating the other's vocabulary.
//!
//! Fail-open, like every other leg here: a query that errors costs its own
//! evidence and nothing else, and a dead daemon costs the whole discovery and
//! is NAMED — never a silent fall back to path names.
//!
//! Test: `super::evidence_tests`.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use super::priority::Priority;

/// Chunk hits requested per query.
const TOP_K: usize = 12;

/// Files one dimension may contribute to the ranking.
const FILES_PER_DIMENSION: usize = 8;

/// Ranked paths written to the manifest, across every source.
///
/// Why: the ranking is an ORDER for the investigation budget to walk, not a
/// second budget — trusty-review still caps files and bytes. This bound only
/// keeps the manifest a readable document.
pub const MAX_PRIORITY_PATHS: usize = 60;

/// Wall-clock budget for one search query.
const REQUEST_BUDGET: Duration = Duration::from_secs(20);

/// Queries derived from the analyst instructions, at most.
const MAX_INSTRUCTION_QUERIES: usize = 4;

/// Longest instruction line accepted as a query.
const MAX_INSTRUCTION_QUERY_CHARS: usize = 160;

/// The dimension whose queries come from the analyst brief rather than this
/// table. Deliberately not one of trusty-review's six: it is a focus the
/// engagement declared, and the report names it as such.
pub const ANALYST_FOCUS: &str = "analyst focus";

/// One DD dimension and the queries that find its evidence.
///
/// Why: the queries ARE the mapping from "what a DD report must assess" to
/// "what an index can answer", and keeping them a `const` table means tuning
/// them is a one-line edit in this crate — the owner's placement ruling
/// (2026-08-19) is that this intelligence lives here, never in trusty-review.
/// What: `dimension` is spelled exactly as trusty-review spells it; `queries`
/// run in order and their hits merge into one per-dimension file list.
/// Test: `super::evidence_tests::every_dimension_matches_the_reviews_spelling`.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct DimensionQueries {
    /// The DD dimension, spelled as trusty-review spells it.
    pub dimension: &'static str,
    /// Semantic queries whose hits count as evidence for that dimension.
    pub queries: &'static [&'static str],
}

/// The six DD dimensions and their queries.
pub const DD_DIMENSIONS: &[DimensionQueries] = &[
    DimensionQueries {
        dimension: "authentication & secrets",
        queries: &[
            "credential handling: api key, token or secret read from configuration or environment",
            "authentication and authorization check on a request path",
            "password hashing, session validation or signature verification",
        ],
    },
    DimensionQueries {
        dimension: "dependencies",
        queries: &[
            "third-party client library wired into the application",
            "dependency version pinning and upgrade handling",
        ],
    },
    DimensionQueries {
        dimension: "state management",
        queries: &[
            "shared mutable state guarded by a lock, mutex or transaction",
            "cache invalidation and consistency after a crash or restart",
        ],
    },
    DimensionQueries {
        dimension: "error handling",
        queries: &[
            "error swallowed: result discarded, empty catch block, ignored failure",
            "panic, unwrap or process exit on a failure path",
            "retry and timeout handling around a fallible call",
        ],
    },
    DimensionQueries {
        dimension: "scalability",
        queries: &[
            "query inside a loop, unbounded collection growth or full scan",
            "connection pool, worker queue and concurrency limits",
        ],
    },
    DimensionQueries {
        dimension: "test coverage",
        queries: &[
            "test asserting the core behaviour of a module",
            "integration test exercising an external boundary",
        ],
    },
];

/// One chunk hit, reduced to what ranking needs.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Hit {
    /// Repo-relative path of the file the chunk came from.
    pub path: String,
    /// The chunk's relevance score, as the daemon reported it.
    pub score: f32,
    /// First line of the chunk, for the reason string.
    pub start_line: usize,
}

/// What one repository's discovery pass found, per dimension.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Discovery {
    /// One entry per dimension that produced at least one hit, in table order.
    pub dimensions: Vec<DimensionEvidence>,
    /// Queries that failed, as one line each, for the caller's gap list.
    pub failures: Vec<String>,
}

/// The files one dimension's queries found, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DimensionEvidence {
    /// The dimension, spelled as trusty-review spells it.
    pub dimension: String,
    /// Repo-relative paths, best score first, with the reason each was chosen.
    pub files: Vec<FileEvidence>,
}

/// One file a dimension's query found, and why it counts as that dimension.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FileEvidence {
    /// Repo-relative path.
    pub path: String,
    /// One line naming the query and score that selected it.
    pub reason: String,
}

/// The search surface discovery needs: one query, its hits.
///
/// Why: a trait rather than a bare function so the ranking is testable without
/// a daemon — the unit tests drive a stub that answers from a table, which is
/// the only way to assert dimension breadth deterministically.
/// What: `hits` answers one query against one index; an `Err` is a reason
/// string, never a panic, because every caller here is fail-open.
/// Test: `super::evidence_tests` drives `StubSearch`; [`HttpSearch`] is the
/// production implementation.
pub trait SearchClient {
    /// Run one query, returning its chunk hits (possibly none).
    fn hits(&self, query: &str) -> impl Future<Output = Result<Vec<Hit>, String>> + Send;
}

/// The trusty-search daemon, over its loopback HTTP surface.
#[derive(Debug, Clone)]
pub struct HttpSearch {
    client: reqwest::Client,
    url: String,
    root: String,
}

impl HttpSearch {
    /// A client for one index, resolving hits against `checkout`.
    ///
    /// The HTTP client is built ONCE here rather than per query: a discovery
    /// pass runs a dozen-plus queries, and each build allocates a fresh
    /// connection pool that the next query then throws away.
    ///
    /// # Errors
    ///
    /// One line when no HTTP client can be built, which the caller turns into a
    /// gap — the discovery leg is fail-open like every other.
    pub fn new(base: &str, index_id: &str, checkout: &Path) -> Result<Self, String> {
        let base = base.trim_end_matches('/');
        let client = trusty_common::http_client::loopback_client_builder()
            .timeout(REQUEST_BUDGET)
            .build()
            .map_err(|e| format!("no HTTP client could be built for {base} ({e})"))?;
        Ok(Self {
            client,
            url: format!("{base}/indexes/{index_id}/search"),
            root: checkout.to_string_lossy().replace('\\', "/"),
        })
    }
}

impl SearchClient for HttpSearch {
    async fn hits(&self, query: &str) -> Result<Vec<Hit>, String> {
        let url = &self.url;
        let response = self
            .client
            .post(url)
            .json(&serde_json::json!({ "text": query, "top_k": TOP_K, "compact": true }))
            .send()
            .await
            .map_err(|e| format!("trusty-search did not answer {url} ({e})"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("trusty-search answered {status} for {url}"));
        }
        let envelope: SearchEnvelope = response
            .json()
            .await
            .map_err(|e| format!("trusty-search answered {url} with an unreadable body ({e})"))?;
        Ok(envelope.into_hits(&self.root))
    }
}

/// The daemon's search response, reduced to the fields ranking reads.
#[derive(Debug, Default, Deserialize)]
struct SearchEnvelope {
    #[serde(default)]
    results: Vec<Chunk>,
}

/// One result row of that response.
#[derive(Debug, Deserialize)]
struct Chunk {
    #[serde(default)]
    file: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    score: f32,
    #[serde(default)]
    start_line: usize,
}

impl SearchEnvelope {
    /// Collapse the response's rows into repo-relative hits.
    fn into_hits(self, root: &str) -> Vec<Hit> {
        self.results
            .into_iter()
            .filter_map(|chunk| {
                let path = relative(&chunk, root)?;
                Some(Hit {
                    path,
                    score: chunk.score,
                    start_line: chunk.start_line,
                })
            })
            .collect()
    }
}

/// One chunk's repo-relative path: the portable `path` when the corpus carries
/// one, else the absolute `file` with the checkout root stripped.
fn relative(chunk: &Chunk, root: &str) -> Option<String> {
    if let Some(portable) = chunk.path.as_deref()
        && !portable.trim().is_empty()
    {
        return Some(portable.replace('\\', "/"));
    }
    if chunk.file.trim().is_empty() {
        return None;
    }
    let file = chunk.file.replace('\\', "/");
    let root = root.trim_end_matches('/');
    Some(
        file.strip_prefix(root)
            .map_or(file.as_str(), |rest| rest.trim_start_matches('/'))
            .to_owned(),
    )
}

/// Queries derived from the analyst's own instructions.
///
/// Why: the brief states what THIS engagement is worried about, and a report
/// that ranks only by the standing six dimensions never looks there. The
/// instructions are prose, so this takes the shape that reads as a topic — a
/// markdown bullet or heading — and leaves the rest alone.
/// What: bullets (`-`, `*`, `1.`) and headings (`#`), trimmed of their marker,
/// deduplicated, each at most [`MAX_INSTRUCTION_QUERY_CHARS`], at most
/// [`MAX_INSTRUCTION_QUERIES`] of them.
/// Test: `super::evidence_tests::instruction_bullets_become_queries`.
#[must_use]
pub fn instruction_queries(instructions: Option<&str>) -> Vec<String> {
    let Some(text) = instructions else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let topic = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("#"))
            .map(str::trim)
            .or_else(|| numbered(trimmed));
        let Some(topic) = topic else { continue };
        let topic = topic.trim_start_matches('#').trim();
        if topic.chars().count() < 8 || topic.chars().count() > MAX_INSTRUCTION_QUERY_CHARS {
            continue;
        }
        let topic = topic.to_owned();
        if !out.contains(&topic) {
            out.push(topic);
        }
        if out.len() >= MAX_INSTRUCTION_QUERIES {
            break;
        }
    }
    out
}

/// `1. topic` / `2) topic` with the marker stripped, when the line is one.
fn numbered(line: &str) -> Option<&str> {
    let (head, rest) = line.split_once(['.', ')'])?;
    if head.is_empty() || !head.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(rest.trim())
}

/// Run every dimension's queries and collect their hits.
///
/// Why: the discovery pass proper. Each dimension asks its own questions, so a
/// file arrives already attributed to the dimension it is evidence FOR — which
/// is what lets the report say why a file was read rather than only that it was.
/// What: queries in table order, then the instruction-derived ones under
/// [`ANALYST_FOCUS`]; per dimension the hits merge by best score and cap at
/// [`FILES_PER_DIMENSION`]. A failing query contributes a line to
/// [`Discovery::failures`] and nothing else.
///
/// # Postconditions
/// Never panics. `dimensions` holds only dimensions with at least one file.
///
/// Test: `super::evidence_tests::{discovery_attributes_each_file_to_its_dimension,
/// a_failing_query_costs_only_its_own_evidence}`.
pub async fn discover<C: SearchClient>(client: &C, instructions: Option<&str>) -> Discovery {
    let mut discovery = Discovery::default();
    let instruction_queries = instruction_queries(instructions);
    let focus: Vec<&str> = instruction_queries.iter().map(String::as_str).collect();
    let sets = DD_DIMENSIONS
        .iter()
        .map(|d| (d.dimension, d.queries.to_vec()))
        .chain(std::iter::once((ANALYST_FOCUS, focus)));

    for (dimension, queries) in sets {
        let mut scored: Vec<(f32, FileEvidence)> = Vec::new();
        for query in queries {
            match client.hits(query).await {
                Ok(hits) => merge(&mut scored, &hits, query),
                Err(cause) => discovery.failures.push(cause),
            }
        }
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored.truncate(FILES_PER_DIMENSION);
        if !scored.is_empty() {
            discovery.dimensions.push(DimensionEvidence {
                dimension: dimension.to_owned(),
                files: scored.into_iter().map(|(_, f)| f).collect(),
            });
        }
    }
    discovery
}

/// Merge one query's hits into a dimension's running best-per-file list.
fn merge(scored: &mut Vec<(f32, FileEvidence)>, hits: &[Hit], query: &str) {
    for hit in hits {
        if hit.path.is_empty() {
            continue;
        }
        match scored.iter_mut().find(|(_, f)| f.path == hit.path) {
            Some(existing) if existing.0 >= hit.score => {}
            Some(existing) => {
                *existing = (
                    hit.score,
                    FileEvidence {
                        path: hit.path.clone(),
                        reason: reason(query, hit),
                    },
                );
            }
            None => scored.push((
                hit.score,
                FileEvidence {
                    path: hit.path.clone(),
                    reason: reason(query, hit),
                },
            )),
        }
    }
}

/// The one-line reason a file is in the ranking.
fn reason(query: &str, hit: &Hit) -> String {
    format!(
        "trusty-search hit for \"{query}\" (score {:.2}, line {})",
        hit.score, hit.start_line
    )
}

/// Interleave the per-dimension evidence with the complexity ranking.
///
/// Why: rank order IS what the investigation budget spends itself on, and
/// ranking one signal ahead of another spends it all there — the complexity-only
/// ranking reached 2 dimensions with 20 files for exactly that reason. Taking
/// one file per dimension per round spreads the same budget across every
/// dimension that has evidence, which is the coverage the report reports.
/// What: round 0 takes the top complexity hotspot then the top file of each
/// dimension, round 1 the next of each, and so on until both are exhausted or
/// `cap` is reached. A path is ranked ONCE, at the earliest position it earns —
/// and when the two legs both name it, the entry carries the dimension as well
/// as both reasons, because it is the dimension that decides whether the report
/// can count that file as covering it.
/// Test: `super::evidence_tests::{blending_spreads_the_budget_across_dimensions,
/// a_hotspot_that_is_also_dimension_evidence_keeps_its_dimension}`.
#[must_use]
pub fn blend(hotspots: &[String], dimensions: &[DimensionEvidence], cap: usize) -> Vec<Priority> {
    let mut out: Vec<Priority> = Vec::new();
    let attributed = attributions(dimensions);
    let depth = dimensions
        .iter()
        .map(|d| d.files.len())
        .chain(std::iter::once(hotspots.len()))
        .max()
        .unwrap_or(0);

    for round in 0..depth {
        if let Some(path) = hotspots.get(round) {
            push(&mut out, hotspot(path, round, &attributed));
        }
        for dimension in dimensions {
            let Some(file) = dimension.files.get(round) else {
                continue;
            };
            push(
                &mut out,
                Priority {
                    path: file.path.clone(),
                    dimension: Some(dimension.dimension.clone()),
                    reason: Some(file.reason.clone()),
                },
            );
        }
        if out.len() >= cap {
            break;
        }
    }
    out.truncate(cap);
    out
}

/// Append a priority unless its path is already ranked.
fn push(out: &mut Vec<Priority>, priority: Priority) {
    if !out.iter().any(|p| p.path == priority.path) {
        out.push(priority);
    }
}

/// Every path the search leg attributed, with its dimension and reason.
///
/// Why: the ranking emits a hotspot BEFORE the dimension entries of the same
/// round and keeps one entry per path, so without this lookup a file that is
/// both the top hotspot and a dimension's evidence would reach the manifest
/// with no dimension at all — and the report would then count that dimension as
/// not investigated even though it read a file addressing it.
/// What: `(path, dimension, reason)` in table order, first occurrence winning,
/// so a path several dimensions found is attributed to the first — one manifest
/// entry carries one dimension.
/// Test: `super::evidence_tests::a_hotspot_that_is_also_dimension_evidence_keeps_its_dimension`.
fn attributions(dimensions: &[DimensionEvidence]) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    for dimension in dimensions {
        for file in &dimension.files {
            if !out.iter().any(|(path, _, _)| path == &file.path) {
                out.push((
                    file.path.clone(),
                    dimension.dimension.clone(),
                    file.reason.clone(),
                ));
            }
        }
    }
    out
}

/// One complexity hotspot, carrying the search attribution when it has one.
fn hotspot(path: &str, round: usize, attributed: &[(String, String, String)]) -> Priority {
    let measured = format!("trusty-analyze complexity hotspot (rank {})", round + 1);
    match attributed.iter().find(|(p, _, _)| p == path) {
        Some((_, dimension, found)) => Priority {
            path: path.to_owned(),
            dimension: Some(dimension.clone()),
            reason: Some(format!("{measured}; {found}")),
        },
        None => Priority {
            path: path.to_owned(),
            dimension: None,
            reason: Some(measured),
        },
    }
}

#[cfg(test)]
#[path = "evidence_tests.rs"]
mod evidence_tests;
