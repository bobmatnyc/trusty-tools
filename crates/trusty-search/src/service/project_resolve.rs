//! Resolve a project — a name, an `owner/repo`, or a path — to its one live
//! index (#9169).
//!
//! Why: index ids are not derivable from a project. A live daemon held
//! `trusty-tools-4e2cf878`, `apex-9a4a584b` and a parked `apex`, so a caller
//! that guessed `trusty-tools` got `unknown index`. Every registration already
//! stores its `repo_identity` and `root_path`, so trusty-search can own the
//! project→index map (ruling f6) instead of each caller guessing.
//!
//! What: [`ProjectQuery::parse`] classifies the input, [`gather_candidates`]
//! builds the candidate list from the persisted registry plus the resident
//! handles without touching disk, and [`resolve`] finds the group the query
//! names and picks one index from it. The pick follows ruling f7: a
//! main-checkout root wins, otherwise the most recently written corpus, and a
//! worktree root never wins. An exact index id or the index owning a path is
//! returned itself unless it is a worktree or orphaned. Every other index of
//! the group is reported in `duplicates`, never dropped. A miss carries the
//! nearest candidates rather than a bare not-found. Disk reads go through
//! [`Disk`] and cover only the matched group and the nearest candidates.
//!
//! Test: `project_resolve_tests.rs`; the RPC adapter in `rpc/project_tests.rs`.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::service::persistence::PersistedIndex;

#[path = "project_resolve_disk.rs"]
mod disk;
pub use disk::{classify_root_kind, Disk, LiveDisk};

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

/// What a registration's root is, as far as ruling f7 cares. Declaration
/// order is the f7 preference order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    /// A repo's main checkout: `.git` is a directory.
    MainCheckout,
    /// An existing root with no `.git` of its own — a subdirectory of a repo,
    /// or a plain directory. Grouped by root, never by repo identity.
    Checkout,
    /// Not inspected: a `/Volumes` root (never `stat`ed), or a candidate the
    /// resolver did not need to probe.
    Indeterminate,
    /// A git worktree: under a worktree base, or `.git` is a file.
    Worktree,
    /// The root no longer exists on disk.
    Orphaned,
}

impl RootKind {
    /// Whether ruling f7 lets an index with this root win a resolve.
    pub fn can_win(self) -> bool {
        !matches!(self, Self::Worktree | Self::Orphaned)
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
    /// How ruling f7 classifies the root; `Indeterminate` until probed.
    pub kind: RootKind,
    /// Whether the index is loaded, as opposed to cold-parked.
    pub resident: bool,
    /// Unix mtime of the index's redb corpus — ruling f7's recency. Read only
    /// for the resolved group; `None` elsewhere and when the corpus is absent.
    pub corpus_modified_unix: Option<u64>,
    /// Whether the registry keeps the corpus under the root (`colocated`).
    #[serde(skip)]
    pub colocated: bool,
}

impl Candidate {
    /// The key of the index's repo: its stored identity, else its root.
    fn identity_key(&self) -> String {
        self.repo_identity
            .clone()
            .unwrap_or_else(|| self.root_key())
    }

    /// The key of the index's root directory.
    fn root_key(&self) -> String {
        format!("root:{}", self.root_path.display())
    }

    /// The key a resolve groups on. #9169: a root with no `.git` of its own is
    /// a subdirectory index, so sibling subdirectories of one repo are never
    /// each other's duplicates.
    fn group_key(&self) -> String {
        if self.kind == RootKind::Checkout {
            self.root_key()
        } else {
            self.identity_key()
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
/// with its identity and storage layout; the resident set says which are
/// loaded. A handle with no persisted row still counts, with no identity.
/// What: one candidate per persisted row, then one per resident handle the
/// registry lacks. No filesystem call: kind and recency are probed later by
/// [`resolve`], and only for the candidates it reports.
/// Test: `a_resident_handle_with_no_persisted_row_is_still_a_candidate`,
/// `the_most_recently_written_corpus_wins_between_two_main_checkouts`.
pub fn gather_candidates(
    persisted: &[PersistedIndex],
    resident: &[(String, PathBuf)],
) -> Vec<Candidate> {
    let unprobed = |id: &str, root: &Path, identity: Option<String>, colocated: bool| Candidate {
        index_id: id.to_string(),
        root_path: root.to_path_buf(),
        repo_identity: identity,
        kind: RootKind::Indeterminate,
        resident: resident.iter().any(|(r, _)| r == id),
        corpus_modified_unix: None,
        colocated,
    };
    let mut out: Vec<Candidate> = persisted
        .iter()
        .map(|e| unprobed(&e.id, &e.root_path, e.repo_identity.clone(), e.colocated))
        .collect();
    for (id, root) in resident {
        if !persisted.iter().any(|e| e.id == *id) {
            out.push(unprobed(id, root, None, false));
        }
    }
    out
}

/// A successful resolve: the one index, how it was matched, and the rest of
/// its group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resolution {
    /// The index ruling f7 picked.
    #[serde(flatten)]
    pub index: Candidate,
    /// `index_id`, `name`, `repo_identity` or `path`.
    pub matched_by: &'static str,
    /// Every other index of the same group, sorted by id.
    pub duplicates: Vec<Candidate>,
}

/// Why a resolve produced no index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveMiss {
    /// Nothing matched; `nearest` holds the closest registrations.
    NotFound { nearest: Vec<Candidate> },
    /// The query matched more than one group.
    Ambiguous { matches: Vec<Candidate> },
    /// The group's only indexes are worktrees or orphaned roots.
    NoLiveIndex { group: Vec<Candidate> },
}

/// Resolve `query` against `candidates` (ruling f6/f7).
///
/// Why: the one place a project becomes an index id, so every caller gets the
/// same answer for the same repo.
/// What: an exact index id, or the deepest registered root containing the
/// (canonical) query path, anchors the resolve: that index wins unless it is a
/// worktree or orphaned, in which case its group's f7 pick wins. A bare name
/// or an identity names a group and the f7 pick wins. A path outside every
/// root falls back to its git identity via [`Disk::derive_identity`].
/// Test: `resolves_by_name_by_identity_and_by_path`,
/// `an_exact_index_id_resolves_to_its_repos_main_checkout`,
/// `an_exact_id_or_owned_path_wins_over_repos_sharing_its_content_identity`,
/// `sibling_subdirectory_indexes_are_never_each_others_duplicates`,
/// `a_dotdot_query_path_matches_the_root_it_names`,
/// `a_name_shared_by_two_repos_is_ambiguous`,
/// `a_miss_reports_the_nearest_candidates`,
/// `a_path_outside_every_root_falls_back_to_the_derived_identity`.
pub fn resolve(
    query: &ProjectQuery,
    candidates: &[Candidate],
    disk: &impl Disk,
) -> Result<Resolution, ResolveMiss> {
    match query {
        ProjectQuery::Identity(id) => resolve_key(id, query, candidates, disk, "repo_identity"),
        ProjectQuery::Name(name) => {
            if let Some(exact) = candidates.iter().find(|c| c.index_id == *name) {
                return resolve_anchored(exact, candidates, disk, "index_id");
            }
            let lower = name.to_lowercase();
            let matches = probed(
                candidates.iter().filter(|c| c.names().contains(&lower)),
                disk,
            );
            match distinct_keys(&matches).as_slice() {
                [] => Err(ResolveMiss::NotFound {
                    nearest: nearest(query, candidates, disk),
                }),
                [only] => resolve_key(only, query, candidates, disk, "name"),
                _ => Err(ResolveMiss::Ambiguous {
                    matches: sorted(matches),
                }),
            }
        }
        ProjectQuery::Path(path) => {
            // #9169: only the query is canonicalised; stored roots already are.
            let query_path = disk
                .canonicalize(path)
                .unwrap_or_else(|| lexical_normalize(path));
            if let Some(owner) = owning_candidate(&query_path, candidates) {
                return resolve_anchored(owner, candidates, disk, "path");
            }
            match disk.derive_identity(path) {
                Some(id) => resolve_key(&id, query, candidates, disk, "path"),
                None => Err(ResolveMiss::NotFound {
                    nearest: nearest(query, candidates, disk),
                }),
            }
        }
    }
}

/// Resolve the group `key` names, picking its winner by ruling f7.
///
/// What: probes the candidates sharing the identity or root `key`. When none
/// of them groups under `key` — every one is a subdirectory index — one root
/// group is used as is and several are ambiguous.
fn resolve_key(
    key: &str,
    query: &ProjectQuery,
    candidates: &[Candidate],
    disk: &impl Disk,
    matched_by: &'static str,
) -> Result<Resolution, ResolveMiss> {
    let members = probed(
        candidates
            .iter()
            .filter(|c| c.identity_key() == key || c.root_key() == key),
        disk,
    );
    let mut group = group_of(&members, key);
    if group.is_empty() {
        match distinct_keys(&members).as_slice() {
            [] => {
                return Err(ResolveMiss::NotFound {
                    nearest: nearest(query, candidates, disk),
                })
            }
            [only] => group = group_of(&members, only),
            _ => {
                return Err(ResolveMiss::Ambiguous {
                    matches: sorted(members),
                })
            }
        }
    }
    pick(with_recency(group, disk), matched_by)
}

/// Resolve from one known index — an exact id or a path's owner (#9169).
///
/// What: the anchor wins whenever its kind can, with the rest of its group in
/// `duplicates`; only a worktree or orphaned anchor yields to its group's f7
/// pick.
fn resolve_anchored(
    anchor: &Candidate,
    candidates: &[Candidate],
    disk: &impl Disk,
    matched_by: &'static str,
) -> Result<Resolution, ResolveMiss> {
    let (identity, root) = (anchor.identity_key(), anchor.root_key());
    let members = probed(
        candidates
            .iter()
            .filter(|c| c.identity_key() == identity || c.root_key() == root),
        disk,
    );
    // The anchor always matches its own identity, so it is among `members`.
    let anchor = members
        .iter()
        .find(|c| c.index_id == anchor.index_id)
        .cloned()
        .unwrap_or_else(|| Candidate {
            kind: disk.kind(anchor),
            ..anchor.clone()
        });
    let group = with_recency(group_of(&members, &anchor.group_key()), disk);
    if !anchor.kind.can_win() {
        return pick(group, matched_by);
    }
    let (index, rest): (Vec<Candidate>, Vec<Candidate>) = group
        .into_iter()
        .partition(|c| c.index_id == anchor.index_id);
    let index = index.into_iter().next().unwrap_or(anchor);
    Ok(Resolution {
        index,
        matched_by,
        duplicates: sorted(rest),
    })
}

/// Ruling f7: pick one index from one group whose recency is filled in.
///
/// What: a worktree or orphaned root never wins. Among the rest a main
/// checkout beats any other root, then the newest corpus wins (an absent
/// corpus sorts last), then the lowest id for determinism.
/// Test: `the_main_checkout_beats_a_newer_indeterminate_root`,
/// `a_worktree_never_wins_even_when_newest`,
/// `an_absent_corpus_sorts_last_among_main_checkouts`,
/// `the_most_recently_written_corpus_wins_between_two_main_checkouts`,
/// `a_repo_with_only_worktree_indexes_has_no_live_index`.
fn pick(group: Vec<Candidate>, matched_by: &'static str) -> Result<Resolution, ResolveMiss> {
    let winner = group
        .iter()
        .filter(|c| c.kind.can_win())
        .min_by(|a, b| {
            a.kind
                .cmp(&b.kind)
                .then(b.corpus_modified_unix.cmp(&a.corpus_modified_unix))
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

/// Clone `candidates` with their root kind read from `disk`.
fn probed<'a>(candidates: impl Iterator<Item = &'a Candidate>, disk: &impl Disk) -> Vec<Candidate> {
    candidates
        .map(|c| Candidate {
            kind: disk.kind(c),
            ..c.clone()
        })
        .collect()
}

/// Fill in each group member's corpus recency.
fn with_recency(mut group: Vec<Candidate>, disk: &impl Disk) -> Vec<Candidate> {
    for c in &mut group {
        c.corpus_modified_unix = disk.corpus_modified_unix(c);
    }
    group
}

/// The probed members whose group key is `key`.
fn group_of(members: &[Candidate], key: &str) -> Vec<Candidate> {
    members
        .iter()
        .filter(|c| c.group_key() == key)
        .cloned()
        .collect()
}

/// The sorted, deduplicated group keys of probed candidates.
fn distinct_keys(list: &[Candidate]) -> Vec<String> {
    let mut keys: Vec<String> = list.iter().map(Candidate::group_key).collect();
    keys.sort();
    keys.dedup();
    keys
}

/// The registration whose root is the deepest ancestor of `path`. No
/// filesystem call: `path` is already canonical (or normalised) and stored
/// roots are canonical.
fn owning_candidate<'a>(path: &Path, candidates: &'a [Candidate]) -> Option<&'a Candidate> {
    candidates
        .iter()
        .filter(|c| path.starts_with(&c.root_path))
        .max_by_key(|c| c.root_path.components().count())
}

/// Resolve `.` and `..` without touching disk, for a path that cannot be
/// canonicalised. #9169: a raw `/w/a/../b` would otherwise match root `/w/a`.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The closest registrations to `query` by Jaro-Winkler similarity.
///
/// What: scores every candidate by name alone, keeps the top
/// [`MAX_NEAREST`], and probes only those, so an equal score can prefer the
/// root f7 would pick.
fn nearest(query: &ProjectQuery, candidates: &[Candidate], disk: &impl Disk) -> Vec<Candidate> {
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
    scored.sort_by(|(sa, a), (sb, b)| sb.total_cmp(sa).then(a.index_id.cmp(&b.index_id)));
    scored.truncate(MAX_NEAREST);
    let mut top: Vec<(f64, Candidate)> = scored
        .into_iter()
        .map(|(s, c)| {
            (
                s,
                Candidate {
                    kind: disk.kind(c),
                    ..c.clone()
                },
            )
        })
        .collect();
    // An equal score prefers the root f7 would pick, so a repo's main checkout
    // leads its own worktrees.
    top.sort_by(|(sa, a), (sb, b)| {
        sb.total_cmp(sa)
            .then(a.kind.cmp(&b.kind))
            .then(a.index_id.cmp(&b.index_id))
    });
    top.into_iter().map(|(_, c)| c).collect()
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
