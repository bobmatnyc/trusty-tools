//! The optional scope `analyze.complexity_hotspots`, `analyze.smells` and
//! `analyze.quality` accept (#10781).
//!
//! Why: a PR review needs the changed files analyzed inside a 60 s budget; a
//! corpus walk takes about two minutes on a 315K-chunk index.
//! What: [`ScopeParams`] adds `paths` (changed files) and `path_prefix` (a
//! directory) to each request. Every entry is one prefix walk over
//! `search.chunks.list` — trusty-search matches at a path-segment boundary,
//! so a file path selects that file. The union of the walks is analyzed; an
//! entry that selected no chunks is reported, never dropped.
//! Test: `analysis_scope_tests.rs`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use futures::stream::{self, StreamExt, TryStreamExt};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::core::ScopeSelectsWholeIndex;
use crate::service::events::{AnalyzerAppState, ApiError};
use crate::types::CodeChunk;

#[cfg(test)]
#[path = "analysis_scope_tests.rs"]
mod tests;

/// Most scope entries one request may carry.
pub const MAX_SCOPE_ENTRIES: usize = 500;

/// Scoped walks in flight at once against trusty-search.
const SCOPE_CONCURRENCY: usize = 4;

/// The additive scope fields, flattened into each request.
///
/// What: both absent means unscoped — the request shape before #10781.
/// `paths: []` with no prefix is a scope that selects nothing, never the
/// whole corpus.
#[derive(Debug, Default, Deserialize)]
pub struct ScopeParams {
    /// Changed file paths, root-relative or absolute.
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    /// One directory or file prefix.
    #[serde(default)]
    pub path_prefix: Option<String>,
}

/// A validated scope.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Scope {
    paths: Vec<String>,
    path_prefix: Option<String>,
}

/// The chunks a scope selected, and the entries that selected none.
pub(crate) struct ScopedChunks {
    pub chunks: Vec<CodeChunk>,
    pub unmatched: Vec<String>,
}

/// `path` with `.` dropped and `..` popped, compared by components.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// True for an entry that names the root with no index root to compare:
/// one that normalizes to nothing (`""`, `.`, `./.`, `a/..`) or to `/`.
fn names_the_root(entry: &str) -> bool {
    let normal = lexical(Path::new(entry));
    normal.as_os_str().is_empty() || normal == Path::new("/")
}

/// True when absolute `entry` resolves to `root`.
///
/// Why (#10781): trusty-search strips its root from an absolute prefix, so
/// `/repo`, `/repo/`, `/repo/.` and a symlinked spelling all become `""`,
/// which matches every chunk.
/// What: compares lexical components, then canonical paths where both exist.
/// Test: `an_absolute_scope_naming_the_root_is_refused`.
fn resolves_to_root(entry: &str, root: &Path) -> bool {
    let (entry, root) = (lexical(Path::new(entry)), lexical(root));
    entry == root
        || matches!(
            (std::fs::canonicalize(&entry), std::fs::canonicalize(&root)),
            (Ok(a), Ok(b)) if a == b
        )
}

/// The `bad_request` for a scope entry naming the index root.
fn root_refusal(raw: &str, field: &str) -> ApiError {
    ApiError::bad_request(format!(
        "`{field}` entry {raw:?} names the index root; omit the scope for a corpus-wide result"
    ))
}

impl ScopeParams {
    /// Validate into a [`Scope`], or `None` when the request is unscoped.
    ///
    /// What: trims each entry; refuses an entry that normalizes to the root
    /// and more than [`MAX_SCOPE_ENTRIES`] entries as `bad_request`; drops
    /// duplicates. An absolute entry is checked against the index root later,
    /// in [`fetch_scoped`].
    /// Test: `scope_validation_refuses_root_and_oversized_scopes`.
    pub(crate) fn resolve(&self) -> Result<Option<Scope>, ApiError> {
        if self.paths.is_none() && self.path_prefix.is_none() {
            return Ok(None);
        }
        let check = |raw: &str, field: &str| -> Result<String, ApiError> {
            let entry = raw.trim();
            if names_the_root(entry) {
                return Err(root_refusal(raw, field));
            }
            Ok(entry.to_string())
        };
        let path_prefix = self
            .path_prefix
            .as_deref()
            .map(|p| check(p, "path_prefix"))
            .transpose()?;
        let mut paths: Vec<String> = Vec::new();
        for raw in self.paths.iter().flatten() {
            let entry = check(raw, "paths")?;
            if !paths.contains(&entry) {
                paths.push(entry);
            }
        }
        let count = paths.len() + usize::from(path_prefix.is_some());
        if count > MAX_SCOPE_ENTRIES {
            return Err(ApiError::bad_request(format!(
                "scope carries {count} entries; at most {MAX_SCOPE_ENTRIES} are accepted"
            )));
        }
        Ok(Some(Scope { paths, path_prefix }))
    }
}

impl Scope {
    /// Every prefix to walk: the `path_prefix` first, then each path.
    fn entries(&self) -> Vec<String> {
        self.path_prefix
            .iter()
            .chain(self.paths.iter())
            .cloned()
            .collect()
    }

    /// Refuse an absolute entry that resolves to the index root (#10781).
    ///
    /// What: an absolute entry needs `root` to be checked; with none it is
    /// refused as `bad_gateway`, because it cannot be shown to be a subset.
    /// Test: `an_absolute_scope_naming_the_root_is_refused`.
    fn refuse_root(&self, root: Option<&str>) -> Result<(), ApiError> {
        let fields = self.path_prefix.iter().map(|p| (p, "path_prefix"));
        for (entry, field) in fields.chain(self.paths.iter().map(|p| (p, "paths"))) {
            if !entry.starts_with('/') {
                continue;
            }
            let Some(root) = root else {
                return Err(ApiError::bad_gateway(format!(
                    "`{field}` entry {entry:?} is absolute, but search.index.status carries \
                     no root_path to check it against"
                )));
            };
            if resolves_to_root(entry, Path::new(root)) {
                return Err(root_refusal(entry, field));
            }
        }
        Ok(())
    }

    /// The `scope` object a scoped response echoes.
    pub(crate) fn echo(&self, unmatched: &[String]) -> Value {
        json!({
            "paths": self.paths,
            "path_prefix": self.path_prefix,
            "unmatched": unmatched,
        })
    }
}

/// Fetch only the chunks `scope` selects.
///
/// What: reads `search.index.status` once and refuses an entry resolving to
/// the index root (#10781). Then one `get_chunks_under` walk per entry, at
/// most [`SCOPE_CONCURRENCY`] at a time, each bounded by the index's chunk
/// count; a walk that selects every chunk is `bad_request`. Chunks are
/// de-duplicated by id and kept in id order, the order a corpus walk yields.
/// Any other walk failure fails the request as `bad_gateway` naming the
/// entry, so a partial scope is never reported as the whole scope.
/// Test: `scoped_hotspots_fetch_only_the_scope`,
/// `a_failed_scope_walk_fails_the_request`,
/// `an_absolute_scope_naming_the_root_is_refused`,
/// `a_scope_the_daemon_widens_to_the_corpus_is_refused`.
pub(crate) async fn fetch_scoped(
    state: &AnalyzerAppState,
    index_id: &str,
    scope: &Scope,
) -> Result<ScopedChunks, ApiError> {
    let entries = scope.entries();
    let mut corpus_chunks = None;
    if !entries.is_empty() {
        let bounds = state
            .search
            .index_scope_bounds(index_id)
            .await
            .map_err(|e| {
                ApiError::bad_gateway(format!(
                    "index '{index_id}': cannot read search.index.status to check the scope \
                     against the index root: {e:#}"
                ))
            })?;
        scope.refuse_root(bounds.root_path.as_deref())?;
        corpus_chunks = bounds.chunk_count;
    }
    let walks: Vec<(String, Vec<CodeChunk>)> = stream::iter(entries)
        .map(|entry| async move {
            let chunks = state
                .search
                .get_chunks_under(index_id, &entry, corpus_chunks)
                .await
                .map_err(|e| {
                    if let Some(widened) = e.downcast_ref::<ScopeSelectsWholeIndex>() {
                        return ApiError::bad_request(widened.to_string());
                    }
                    tracing::warn!("scoped get_chunks({index_id}, {entry:?}) failed: {e:#}");
                    ApiError::bad_gateway(format!("get_chunks({index_id}) under {entry:?}: {e:#}"))
                })?;
            Ok::<_, ApiError>((entry, chunks))
        })
        .buffered(SCOPE_CONCURRENCY)
        .try_collect()
        .await?;
    let mut by_id: BTreeMap<String, CodeChunk> = BTreeMap::new();
    let mut unmatched = Vec::new();
    for (entry, chunks) in walks {
        if chunks.is_empty() {
            unmatched.push(entry);
        }
        for chunk in chunks {
            by_id.entry(chunk.id.clone()).or_insert(chunk);
        }
    }
    Ok(ScopedChunks {
        chunks: by_id.into_values().collect(),
        unmatched,
    })
}
