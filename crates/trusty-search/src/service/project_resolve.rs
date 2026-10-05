//! Resolve a project — a name, an `owner/repo`, or a path — to its one live
//! index (#9169).
//!
//! Why: index ids are not derivable from a project. A live daemon held
//! `trusty-tools-4e2cf878`, `apex-9a4a584b` and a parked `apex`, so a caller
//! that guessed `trusty-tools` got `unknown index`. Every registration already
//! stores its `repo_identity` and `root_path`, so trusty-search can own the
//! project→index map (ruling f6) instead of each caller guessing.
//!
//! What: [`ProjectQuery::parse`] classifies the input, [`resolve`] finds the
//! repo group it names and picks one index from it, and [`gather_candidates`]
//! builds the candidate list from the persisted registry plus the resident
//! handles. The pick follows ruling f7: a main-checkout root wins, otherwise
//! the most recently indexed root, and a worktree root never wins. Every other
//! index of the same repo is reported in `duplicates`, never dropped. A miss
//! carries the nearest candidates rather than a bare not-found.
//!
//! Test: `project_resolve_tests.rs`; the RPC adapter in `rpc/project_tests.rs`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::service::persistence::PersistedIndex;

/// How many nearest candidates a miss reports.
const MAX_NEAREST: usize = 5;

/// What a caller asked to resolve, after classifying its one input string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectQuery {
    /// An absolute (or `~`-relative) filesystem path.
    Path(PathBuf),
    /// A canonical repo identity: `owner/repo` or `content:<sha>`.
    Identity(String),
    /// A bare project name or an index id.
    Name(String),
}

impl ProjectQuery {
    /// Classify `raw`: a leading `/` or `~` is a path, a `/` or `content:`
    /// elsewhere is a repo identity, anything else is a name.
    ///
    /// # Errors
    ///
    /// An empty input, or one with a `/` that is neither absolute nor an
    /// `owner/repo` — the daemon's working directory is not the caller's, so a
    /// relative path cannot be resolved.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("project must not be empty".to_string());
        }
        if trimmed.starts_with('/') {
            return Ok(Self::Path(PathBuf::from(trimmed)));
        }
        if let Some(rest) = trimmed.strip_prefix('~') {
            if rest.is_empty() || rest.starts_with('/') {
                let home = dirs::home_dir().ok_or("cannot expand `~`: no home directory")?;
                return Ok(Self::Path(home.join(rest.trim_start_matches('/'))));
            }
        }
        if trimmed.contains('/') || trimmed.starts_with("content:") {
            if trimmed.starts_with('.') {
                return Err(format!(
                    "{trimmed:?} is a relative path; send an absolute path instead"
                ));
            }
            return trusty_common::repo_identity::RepoIdentity::parse(trimmed)
                .map(|id| Self::Identity(id.canonical()))
                .ok_or_else(|| format!("{trimmed:?} is not an owner/repo identity"));
        }
        Ok(Self::Name(trimmed.to_string()))
    }
}

/// What a registration's root is, as far as ruling f7 cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    /// A repo's main checkout: `.git` is a directory.
    MainCheckout,
    /// An existing root that is neither a main checkout nor a worktree.
    Checkout,
    /// A git worktree: under a worktree base, or `.git` is a file.
    Worktree,
    /// The root no longer exists on disk.
    Orphaned,
}

impl RootKind {
    /// Whether ruling f7 lets an index with this root win a resolve.
    pub fn can_win(self) -> bool {
        matches!(self, Self::MainCheckout | Self::Checkout)
    }
}

/// Classify `root` for ruling f7.
///
/// What: a path component naming a worktree base (`.worktrees`, the configured
/// base, or `.claude/worktrees`) is a worktree whether or not it still exists;
/// otherwise a missing root is orphaned, a `.git` file marks a linked worktree,
/// and a `.git` directory marks a main checkout.
/// Test: `classify_root_kind_reads_the_git_entry_and_the_worktree_base`.
pub fn classify_root_kind(
    root: &Path,
    worktree_names: &trusty_common::workspace_layout::WorktreeDirNames,
) -> RootKind {
    let mut previous: Option<&std::ffi::OsStr> = None;
    for component in root.components() {
        let name = component.as_os_str();
        let claude_worktree = previous == Some(std::ffi::OsStr::new(".claude"))
            && name == std::ffi::OsStr::new("worktrees");
        if claude_worktree || name.to_str().is_some_and(|n| worktree_names.matches(n)) {
            return RootKind::Worktree;
        }
        previous = Some(name);
    }
    if matches!(std::fs::symlink_metadata(root), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
    {
        return RootKind::Orphaned;
    }
    match std::fs::metadata(root.join(".git")) {
        Ok(meta) if meta.is_file() => RootKind::Worktree,
        Ok(meta) if meta.is_dir() => RootKind::MainCheckout,
        _ => RootKind::Checkout,
    }
}

/// One registration a resolve can pick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    /// The index id.
    pub index_id: String,
    /// The index's root directory, rendered lossily on the wire (#5827: a
    /// non-UTF-8 path must not fail serialisation).
    #[serde(serialize_with = "lossy_path")]
    pub root_path: PathBuf,
    /// The stored canonical repo identity, when one was derived.
    pub repo_identity: Option<String>,
    /// How ruling f7 classifies the root.
    pub kind: RootKind,
    /// Whether the index is loaded, as opposed to cold-parked.
    pub resident: bool,
    /// Unix time of the last completed reindex, when known.
    pub last_indexed_unix: Option<u64>,
}

impl Candidate {
    /// The key a repo group is formed on: the identity, else the root path.
    fn group_key(&self) -> String {
        match &self.repo_identity {
            Some(id) => id.clone(),
            None => format!("root:{}", self.root_path.display()),
        }
    }

    /// Every name a bare-name query may match, lowercased.
    fn names(&self) -> Vec<String> {
        let mut names = vec![self.index_id.to_lowercase()];
        names.push(strip_hash_suffix(&self.index_id).to_lowercase());
        if let Some(base) = self.root_path.file_name().and_then(|b| b.to_str()) {
            names.push(base.to_lowercase());
        }
        if let Some(repo) = self
            .repo_identity
            .as_deref()
            .filter(|id| !id.starts_with("content:"))
            .and_then(|id| id.rsplit('/').next())
        {
            names.push(repo.to_lowercase());
        }
        names
    }
}

/// `trusty-tools-4e2cf878` → `trusty-tools`: drop a `-<8 hex>` id suffix.
fn strip_hash_suffix(id: &str) -> &str {
    match id.rsplit_once('-') {
        Some((stem, hash))
            if !stem.is_empty()
                && hash.len() == 8
                && hash.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            stem
        }
        _ => id,
    }
}

/// Build the candidate list from the persisted registry and the resident ids.
///
/// Why: `indexes.toml` holds every registration — resident and cold-parked —
/// with its identity and recency; the resident set says which are loaded. A
/// handle with no persisted row still counts, with no identity.
/// What: one candidate per persisted row, then one per resident handle the
/// registry lacks. `classify` is injected so tests need no real directories.
/// Test: `a_resident_handle_with_no_persisted_row_is_still_a_candidate`.
pub fn gather_candidates(
    persisted: &[PersistedIndex],
    resident: &[(String, PathBuf)],
    classify: impl Fn(&Path) -> RootKind,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = persisted
        .iter()
        .map(|e| Candidate {
            index_id: e.id.clone(),
            root_path: e.root_path.clone(),
            repo_identity: e.repo_identity.clone(),
            kind: classify(&e.root_path),
            resident: resident.iter().any(|(id, _)| *id == e.id),
            last_indexed_unix: e.last_indexed_unix,
        })
        .collect();
    for (id, root) in resident {
        if !persisted.iter().any(|e| e.id == *id) {
            out.push(Candidate {
                index_id: id.clone(),
                root_path: root.clone(),
                repo_identity: None,
                kind: classify(root),
                resident: true,
                last_indexed_unix: None,
            });
        }
    }
    out
}

/// A successful resolve: the one index, how it was matched, and the rest of
/// its repo group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resolution {
    /// The index ruling f7 picked.
    #[serde(flatten)]
    pub index: Candidate,
    /// `index_id`, `name`, `repo_identity` or `path`.
    pub matched_by: &'static str,
    /// Every other index of the same repo, sorted by id.
    pub duplicates: Vec<Candidate>,
}

/// Why a resolve produced no index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveMiss {
    /// Nothing matched; `nearest` holds the closest registrations.
    NotFound { nearest: Vec<Candidate> },
    /// A name matched indexes of more than one repo.
    Ambiguous { matches: Vec<Candidate> },
    /// The repo's only indexes are worktrees or orphaned roots.
    NoLiveIndex { group: Vec<Candidate> },
}

/// Resolve `query` against `candidates` (ruling f6/f7).
///
/// Why: the one place a project becomes an index id, so every caller gets the
/// same answer for the same repo.
/// What: finds the repo group the query names — exact index id, then a bare
/// name, an identity, or the deepest registered root containing a path, with
/// `derive` (a git read) as the fallback for a path outside every root — then
/// picks the winner with [`pick`]. `derive` runs only on that fallback.
/// Test: `resolves_by_name_by_identity_and_by_path`,
/// `an_exact_index_id_resolves_to_its_repos_main_checkout`,
/// `a_name_shared_by_two_repos_is_ambiguous`,
/// `a_miss_reports_the_nearest_candidates`,
/// `a_path_outside_every_root_falls_back_to_the_derived_identity`.
pub fn resolve(
    query: &ProjectQuery,
    candidates: &[Candidate],
    derive: impl FnOnce(&Path) -> Option<String>,
) -> Result<Resolution, ResolveMiss> {
    let (key, matched_by) = match query {
        ProjectQuery::Identity(id) => (Some(id.clone()), "repo_identity"),
        ProjectQuery::Name(name) => {
            if let Some(exact) = candidates.iter().find(|c| c.index_id == *name) {
                (Some(exact.group_key()), "index_id")
            } else {
                let lower = name.to_lowercase();
                let matches: Vec<&Candidate> = candidates
                    .iter()
                    .filter(|c| c.names().contains(&lower))
                    .collect();
                let mut keys: Vec<String> = matches.iter().map(|c| c.group_key()).collect();
                keys.sort();
                keys.dedup();
                if keys.len() > 1 {
                    return Err(ResolveMiss::Ambiguous {
                        matches: sorted(matches.into_iter().cloned().collect()),
                    });
                }
                (keys.pop(), "name")
            }
        }
        ProjectQuery::Path(path) => match owning_candidate(path, candidates) {
            Some(owner) => (Some(owner.group_key()), "path"),
            None => (derive(path), "path"),
        },
    };
    let group: Vec<Candidate> = match &key {
        Some(key) => candidates
            .iter()
            .filter(|c| c.group_key() == *key)
            .cloned()
            .collect(),
        None => Vec::new(),
    };
    if group.is_empty() {
        return Err(ResolveMiss::NotFound {
            nearest: nearest(query, candidates),
        });
    }
    pick(group, matched_by)
}

/// Ruling f7: pick one index from one repo group.
///
/// What: a worktree or orphaned root never wins. Among the rest a main
/// checkout beats any other root, then the most recent `last_indexed_unix`
/// wins (never-indexed sorts last), then the lowest id for determinism.
/// Test: `the_main_checkout_beats_a_newer_plain_checkout`,
/// `a_worktree_never_wins_even_when_newest`,
/// `recency_breaks_a_tie_between_two_plain_checkouts`,
/// `a_repo_with_only_worktree_indexes_has_no_live_index`.
fn pick(group: Vec<Candidate>, matched_by: &'static str) -> Result<Resolution, ResolveMiss> {
    let winner = group
        .iter()
        .filter(|c| c.kind.can_win())
        .min_by(|a, b| {
            a.kind
                .cmp(&b.kind)
                .then(b.last_indexed_unix.cmp(&a.last_indexed_unix))
                .then(a.index_id.cmp(&b.index_id))
        })
        .cloned();
    let Some(index) = winner else {
        return Err(ResolveMiss::NoLiveIndex {
            group: sorted(group),
        });
    };
    let duplicates = sorted(
        group
            .into_iter()
            .filter(|c| c.index_id != index.index_id)
            .collect(),
    );
    Ok(Resolution {
        index,
        matched_by,
        duplicates,
    })
}

/// The registration whose root is the deepest ancestor of `path`.
fn owning_candidate<'a>(path: &Path, candidates: &'a [Candidate]) -> Option<&'a Candidate> {
    let canonical = std::fs::canonicalize(path).ok();
    candidates
        .iter()
        .filter(|c| {
            let root_canonical = std::fs::canonicalize(&c.root_path).ok();
            [Some(path), canonical.as_deref()]
                .into_iter()
                .flatten()
                .any(|p| {
                    p.starts_with(&c.root_path)
                        || root_canonical.as_deref().is_some_and(|r| p.starts_with(r))
                })
        })
        .max_by_key(|c| c.root_path.components().count())
}

/// The closest registrations to `query` by Jaro-Winkler similarity.
fn nearest(query: &ProjectQuery, candidates: &[Candidate]) -> Vec<Candidate> {
    let needle = match query {
        ProjectQuery::Name(name) => name.to_lowercase(),
        ProjectQuery::Identity(id) => id.rsplit('/').next().unwrap_or(id).to_lowercase(),
        ProjectQuery::Path(path) => path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_lowercase(),
    };
    let mut scored: Vec<(f64, &Candidate)> = candidates
        .iter()
        .map(|c| {
            let score = c
                .names()
                .iter()
                .map(|n| strsim::jaro_winkler(&needle, n))
                .fold(0.0_f64, f64::max);
            (score, c)
        })
        .collect();
    // An equal score prefers the root f7 would pick, so a repo's main checkout
    // leads its own worktrees.
    scored.sort_by(|(sa, a), (sb, b)| {
        sb.total_cmp(sa)
            .then(a.kind.cmp(&b.kind))
            .then(a.index_id.cmp(&b.index_id))
    });
    scored
        .into_iter()
        .take(MAX_NEAREST)
        .map(|(_, c)| c.clone())
        .collect()
}

/// Serialise a path as text, replacing any non-UTF-8 bytes.
fn lossy_path<S: serde::Serializer>(path: &Path, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&path.to_string_lossy())
}

/// Sort candidates by id so every reported list is deterministic.
fn sorted(mut list: Vec<Candidate>) -> Vec<Candidate> {
    list.sort_by(|a, b| a.index_id.cmp(&b.index_id));
    list
}

#[cfg(test)]
#[path = "project_resolve_tests.rs"]
mod tests;
