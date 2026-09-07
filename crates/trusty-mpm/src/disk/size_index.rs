//! Incremental, cached directory-size index (#6926, DOC-73 §16.6 item 1).
//!
//! Why: the Disk dashboard renders every registered project and every worktree
//! under it as a sunburst. Measuring that cold on each load re-stats every file
//! in every `target/`, `node_modules/` and `.git/` in the workspace — the exact
//! cost DOC-73 §16.1 says a TiB-scale view cannot pay. Neither existing
//! primitive helps: `trusty_common::sys_metrics::dir_size_bytes` and this
//! crate's `worktree_reclaim::measure_bytes_until` both walk the whole tree on
//! every call and keep nothing.
//! What: [`DirSizeIndex`] caches two things — a per-ROOT total with a
//! timestamp, returned with zero syscalls inside [`IndexPolicy::max_age`], and
//! a per-DIRECTORY node (its own file bytes, its child directories, its mtime)
//! so a refresh re-reads only the directories that actually changed.
//! Test: `size_index_tests`.
//!
//! # The design, where the spec is silent
//!
//! DOC-73 §16.6 asks for "a background or on-demand cache mapping worktree path
//! to bytes-with-timestamp, refreshed incrementally rather than walking cold on
//! every load" and names no mechanism, cadence, or eviction rule. The choices
//! made here, and why:
//!
//! - **On-demand, not background.** Nothing schedules a walk. A caller asks for
//!   a path and either gets the cached total or pays for one incremental
//!   refresh. A background ticker would walk trees nobody is looking at.
//! - **Directory mtime is the invalidation signal.** A directory's mtime
//!   changes when an entry is created, deleted, or renamed inside it, so
//!   comparing it against the cached value tells us whether that directory's
//!   own listing can be reused. Revalidating a subtree therefore costs one
//!   `lstat` per DIRECTORY instead of one per FILE, which is the whole win: a
//!   `target/` with 200k files across 5k directories revalidates in 5k stats.
//!   fsevents was not used — it needs a running watcher, a daemon lifetime, and
//!   a recovery story for dropped events, none of which a
//!   measured-when-asked figure justifies.
//! - **A second, longer bound catches what mtime cannot.** Appending to an
//!   existing file changes that FILE's mtime, not its directory's, so
//!   mtime-only revalidation would never see a log or database file grow in
//!   place. [`IndexPolicy::node_max_age`] forces a node to be re-read once it
//!   is that old regardless of mtime, which bounds that staleness instead of
//!   leaving it unbounded.
//! - **The cadence is 60s**, matching the ticker `dir_size_bytes` already runs
//!   on for the daemon's `/health` `disk_bytes`. Callers that need to know how
//!   old a figure is read [`DirSize::measured_at`], which §16.5's
//!   `generated_at` field is for.
//! - **Monotonic clock for the cadence, wall clock for display.** Ages are
//!   compared with [`Instant`], so a clock jump cannot make a cached total look
//!   fresh forever; [`DirSize::measured_at`] is a [`SystemTime`] purely so the
//!   dashboard can render it.
//!
//! # Bounds, because a size walk is an unbounded operation by default
//!
//! - [`IndexPolicy::forbidden_roots`] refuses `/` and `$HOME` — and any
//!   ancestor of them — before any walk begins. The comparison is made on
//!   RESOLVED paths (`canonicalize`, falling back to a lexical `.`/`..` fold),
//!   so a `..`-laced spelling of a forbidden root is refused exactly like the
//!   direct one.
//! - Symlinks are never followed and never counted, so a link out of a worktree
//!   can neither inflate the figure nor loop.
//! - [`IndexPolicy::max_depth`] caps descent; [`IndexPolicy::walk_budget`] caps
//!   wall time. Either bound sets [`DirSize::truncated`] and returns the
//!   partial total rather than failing.
//! - A directory the OS refuses is recorded in [`DirSize::unreadable`] and the
//!   walk continues. It is never cached, so the next refresh retries it.
//!
//! # Known duplication, for whoever touches either walker next
//!
//! [`sum_entries`]'s per-entry classification (skip symlink, recurse into a
//! directory, add a regular file's length) repeats the inner loop of
//! `trusty_common::sys_metrics`'s private `walk_bounded`. Folding both onto one
//! shared `sys_metrics` primitive is follow-up work that ships inside the next
//! change touching either side, not a standalone cleanup.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How long a cached ROOT total is served without touching the filesystem.
///
/// Matches the 60-second ticker `dir_size_bytes` already runs on for the
/// daemon's `/health` `disk_bytes`, so the Disk dashboard is no staler than the
/// figure the daemon already publishes.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(60);

/// How long a cached DIRECTORY node is trusted on an unchanged mtime.
///
/// Why: directory mtime does not move when an existing file grows in place, so
/// mtime alone would never notice a log or database file expanding. Fifteen
/// minutes bounds that blind spot while still letting the common case — a tree
/// nobody touched — revalidate at one stat per directory.
pub const DEFAULT_NODE_MAX_AGE: Duration = Duration::from_secs(900);

/// Deepest directory level the walk will open, counting the root as level 0.
///
/// Same value and same reasoning as `dir_size_bytes`'s own cap (#4764): real
/// worktrees nest well under ten levels, so this only trips on something
/// already pathological.
pub const DEFAULT_MAX_DEPTH: usize = 64;

/// Wall-clock ceiling on one refresh.
///
/// A refresh that has run this long is contending with build churn rather than
/// measuring it; the partial total plus [`DirSize::truncated`] is strictly more
/// useful than holding the thread.
pub const DEFAULT_WALK_BUDGET: Duration = Duration::from_secs(30);

/// Directories opened between two clock reads during a refresh.
const BUDGET_CHECK_INTERVAL: usize = 256;

/// The bounds and cadences one [`DirSizeIndex`] runs under.
///
/// Why: every field here is a bound on an operation that is unbounded by
/// default, so they are stated in one place a caller can read and a test can
/// pin rather than scattered as constants inside the walk.
/// What: the two staleness windows, the two walk bounds, and the root
/// allowlist's inverse.
/// Test: `default_policy_forbids_the_filesystem_root`,
/// `a_forbidden_root_is_refused_before_any_walk`.
#[derive(Debug, Clone)]
pub struct IndexPolicy {
    /// Serve a cached root total younger than this with no syscalls at all.
    pub max_age: Duration,
    /// Re-read a directory older than this even when its mtime is unchanged.
    pub node_max_age: Duration,
    /// Deepest level to open, root counted as 0.
    pub max_depth: usize,
    /// Wall-clock ceiling on one refresh.
    pub walk_budget: Duration,
    /// Paths that may never be indexed, nor may any ancestor of them be.
    pub forbidden_roots: Vec<PathBuf>,
}

impl Default for IndexPolicy {
    fn default() -> Self {
        Self {
            max_age: DEFAULT_MAX_AGE,
            node_max_age: DEFAULT_NODE_MAX_AGE,
            max_depth: DEFAULT_MAX_DEPTH,
            walk_budget: DEFAULT_WALK_BUDGET,
            forbidden_roots: default_forbidden_roots(),
        }
    }
}

/// The roots no default-configured index will ever walk.
///
/// Why: "measure this directory" pointed at `/` or `$HOME` is a whole-machine
/// sweep. Refusing both by default means a caller has to opt in explicitly to
/// ask for one, rather than reaching it through a mistake.
/// What: the filesystem root, plus `$HOME` when the environment names one.
/// Test: `default_policy_forbids_the_filesystem_root`.
fn default_forbidden_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/")];
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if !home.as_os_str().is_empty() {
            roots.push(home);
        }
    }
    roots
}

/// Why a measurement could not even be attempted.
///
/// Why: these two are refusals, not failures — the index declined to walk. A
/// walk that STARTS and hits a bound or an unreadable subtree returns a partial
/// [`DirSize`] instead, because a partial disk figure is still a disk figure.
/// Test: `a_forbidden_root_is_refused_before_any_walk`,
/// `a_file_path_is_not_a_directory`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SizeIndexError {
    /// The path is a forbidden root, or an ancestor of one.
    #[error(
        "refusing to index `{path}`: it is, or contains, a root this index never walks by default"
    )]
    ForbiddenRoot {
        /// The path as the caller supplied it.
        path: PathBuf,
    },
    /// The path is missing, or is not a directory (a symlink included).
    #[error("refusing to index `{path}`: not a directory")]
    NotADirectory {
        /// The path as the caller supplied it.
        path: PathBuf,
    },
}

/// One measured directory total, with everything needed to judge it.
///
/// Why: DOC-73 §16.5's `generated_at` exists because a cached figure must
/// disclose its own age, and §16.1 notes the existing primitives silently
/// return partial totals. This struct carries the age AND the two ways the
/// number can be short, so no caller has to guess. Both shortfalls ride on the
/// cached record as well, so a cache hit never launders a partial figure into a
/// clean one.
/// Test: `a_second_read_inside_the_cadence_performs_no_walk`,
/// `an_unreadable_subtree_is_recorded_not_fatal`,
/// `a_cached_read_still_reports_truncation`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DirSize {
    /// The root that was measured, exactly as the caller supplied it.
    pub path: PathBuf,
    /// Sum of regular-file lengths beneath `path`, symlinks excluded.
    pub bytes: u64,
    /// When the walk behind this figure ran. Not "now" for a cached hit.
    pub measured_at: SystemTime,
    /// A depth cap or the wall-clock budget cut the walk short.
    pub truncated: bool,
    /// Directories the OS refused, whose contents are therefore uncounted.
    pub unreadable: Vec<PathBuf>,
    /// This figure was served from cache without touching the filesystem.
    pub from_cache: bool,
}

/// Cumulative counters, and the only way to prove the cache is a cache.
///
/// Why: "does not re-walk" is not observable from a byte total — the same
/// number comes back either way. These counters are what
/// `a_second_read_inside_the_cadence_performs_no_walk` asserts on, and what
/// makes the incremental claim testable rather than asserted.
/// What: `directories_read` counts `read_dir` calls, `directories_revalidated`
/// counts directories whose cached listing was reused after an mtime check,
/// `cache_hits` counts measurements answered with no syscall at all.
/// Test: `a_second_read_inside_the_cadence_performs_no_walk`,
/// `a_changed_subtree_is_the_only_one_re_read`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct IndexStats {
    /// Directories whose entries were listed with `read_dir`.
    pub directories_read: u64,
    /// Directories reused on an unchanged mtime, costing one stat.
    pub directories_revalidated: u64,
    /// Measurements answered from the root cache, costing nothing.
    pub cache_hits: u64,
    /// Measurements that ran a refresh walk.
    pub refreshes: u64,
}

/// One cached directory: what it holds directly, and what hangs off it.
#[derive(Debug)]
struct DirNode {
    /// Directory mtime when its entries were last listed.
    mtime: Option<SystemTime>,
    /// Sum of the direct regular-file lengths in this directory.
    own_bytes: u64,
    /// Immediate child directories, absolute.
    children: Vec<PathBuf>,
    /// When `read_dir` last ran here — the [`IndexPolicy::node_max_age`] clock.
    read_at: Instant,
}

/// One cached root total.
#[derive(Debug)]
struct RootRecord {
    bytes: u64,
    measured_at: SystemTime,
    read_at: Instant,
    truncated: bool,
    unreadable: Vec<PathBuf>,
}

/// A path → bytes-with-timestamp cache whose refresh is incremental.
///
/// Why: see the module docs — a TiB-scale sunburst cannot walk cold on every
/// load, and DOC-73 §16.6 item 1 makes this the prerequisite for the rest of
/// the Disk work.
/// What: [`measure`](Self::measure) is the whole surface. Inside
/// [`IndexPolicy::max_age`] it answers from the root cache with no syscalls;
/// outside it, it re-reads only the directories whose mtime moved, reusing
/// every other cached node at one stat apiece.
///
/// Not `Sync`-shared: hold it behind the caller's own lock. Nothing here spawns
/// or blocks on anything, so a `Mutex` around it is enough.
/// Test: `size_index_tests`.
#[derive(Debug)]
pub struct DirSizeIndex {
    policy: IndexPolicy,
    nodes: HashMap<PathBuf, DirNode>,
    roots: HashMap<PathBuf, RootRecord>,
    stats: IndexStats,
}

impl Default for DirSizeIndex {
    fn default() -> Self {
        Self::with_policy(IndexPolicy::default())
    }
}

impl DirSizeIndex {
    /// An empty index under [`IndexPolicy::default`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty index under an explicit policy.
    ///
    /// The forbidden roots are resolved here, once, so every later
    /// [`guard_root`](Self::guard_root) comparison has both sides in the same
    /// form without re-resolving a fixed list on every call. The policy this
    /// index reports through [`policy`](Self::policy) therefore holds the
    /// RESOLVED roots, not the spellings the caller passed.
    /// Test: `a_forbidden_root_is_refused_before_any_walk`,
    /// `a_dot_dot_path_into_a_forbidden_root_is_refused`.
    #[must_use]
    pub fn with_policy(mut policy: IndexPolicy) -> Self {
        policy.forbidden_roots = policy
            .forbidden_roots
            .iter()
            .map(|root| resolve_for_guard(root))
            .collect();
        Self {
            policy,
            nodes: HashMap::new(),
            roots: HashMap::new(),
            stats: IndexStats::default(),
        }
    }

    /// The counters. See [`IndexStats`] for why they exist.
    #[must_use]
    pub fn stats(&self) -> IndexStats {
        self.stats
    }

    /// The bounds this index runs under.
    #[must_use]
    pub fn policy(&self) -> &IndexPolicy {
        &self.policy
    }

    /// Drop every cached fact about `root` and everything under it.
    ///
    /// Why: the Disk dashboard's clear action (§16.4) deletes a worktree, and a
    /// cached total for a path that no longer exists would survive for a whole
    /// `max_age` window afterwards.
    /// Test: `invalidate_forces_the_next_read_to_walk`.
    pub fn invalidate(&mut self, root: &Path) {
        self.roots.remove(root);
        self.nodes.retain(|path, _| !path.starts_with(root));
    }

    /// Bytes under `root`, from cache when fresh and from an incremental
    /// refresh when not.
    ///
    /// Why: this is the one entry point, so the cache can never be bypassed by
    /// accident — there is no uncached spelling to reach for.
    /// What: refuses a forbidden or non-directory root outright; otherwise
    /// serves a root record younger than [`IndexPolicy::max_age`] with no
    /// syscalls, or walks, reusing every directory whose mtime is unchanged and
    /// young enough. Bounds and unreadable subtrees surface on the returned
    /// [`DirSize`] rather than as errors.
    ///
    /// # Errors
    ///
    /// [`SizeIndexError::ForbiddenRoot`] when `root` is, or contains, a
    /// forbidden root; [`SizeIndexError::NotADirectory`] when it is missing or
    /// is not a directory. Both are decided before any walk begins.
    ///
    /// Test: `a_second_read_inside_the_cadence_performs_no_walk`,
    /// `a_changed_subtree_is_the_only_one_re_read`,
    /// `bytes_sum_the_files_in_a_known_tree`,
    /// `a_forbidden_root_is_refused_before_any_walk`.
    pub fn measure(&mut self, root: &Path) -> Result<DirSize, SizeIndexError> {
        self.guard_root(root)?;
        let root = root.to_path_buf();

        if let Some(record) = self.roots.get(&root)
            && record.read_at.elapsed() < self.policy.max_age
        {
            self.stats.cache_hits += 1;
            return Ok(record.to_dir_size(root, true));
        }

        let record = self.refresh(&root);
        let size = record.to_dir_size(root.clone(), false);
        self.roots.insert(root, record);
        Ok(size)
    }

    /// Reject `root` before any syscall runs.
    ///
    /// What: a forbidden root is matched in the containing direction — a
    /// candidate that is an ANCESTOR of `$HOME` (`/Users`) is refused too,
    /// because walking it walks `$HOME`.
    ///
    /// Both sides of that comparison are resolved first. `Path::starts_with` is
    /// purely lexical, so an unresolved `$HOME/x/../..` compares as unrelated to
    /// `$HOME` while naming its parent — the guard would pass and the walk would
    /// start above the root it was told to refuse. Candidate and forbidden roots
    /// therefore both go through [`resolve_for_guard`], and the forbidden set is
    /// resolved once in [`DirSizeIndex::with_policy`] rather than per call.
    ///
    /// Directory-ness is checked FIRST, on the LITERAL path, so a symlinked root
    /// is refused as [`SizeIndexError::NotADirectory`] rather than resolved into
    /// its target; "symlinks are never followed" then holds without a root
    /// exception, even though the guard's own comparison resolves them.
    /// Test: `a_dot_dot_path_into_a_forbidden_root_is_refused`,
    /// `a_forbidden_root_is_refused_before_any_walk`,
    /// `a_file_path_is_not_a_directory`.
    fn guard_root(&self, root: &Path) -> Result<(), SizeIndexError> {
        let is_dir = std::fs::symlink_metadata(root).is_ok_and(|md| md.is_dir());
        if !is_dir {
            return Err(SizeIndexError::NotADirectory {
                path: root.to_path_buf(),
            });
        }
        // #6926: compare resolved forms — a `..`-laced spelling of a forbidden
        // root is the same directory and must be refused the same way.
        let resolved = resolve_for_guard(root);
        if self
            .policy
            .forbidden_roots
            .iter()
            .any(|forbidden| forbidden.starts_with(&resolved))
        {
            return Err(SizeIndexError::ForbiddenRoot {
                path: root.to_path_buf(),
            });
        }
        Ok(())
    }

    /// One incremental refresh of the subtree at `root`.
    ///
    /// What: an explicit stack, holding at most one `ReadDir` at a time (the
    /// #4764 discipline `dir_size_bytes` documents — a directory handle is
    /// drained and dropped before any child is opened). Each popped directory
    /// is either revalidated (mtime matches a young cached node: reuse its
    /// bytes and children, one stat) or re-read (`read_dir`). Totals are then
    /// summed bottom-up over the visit order, which is a DFS pre-order, so
    /// reversing it guarantees every child is folded in before its parent.
    /// Test: `a_changed_subtree_is_the_only_one_re_read`,
    /// `a_deleted_subtree_drops_out_of_the_total`.
    fn refresh(&mut self, root: &Path) -> RootRecord {
        self.stats.refreshes += 1;
        let started = Instant::now();
        let mut truncated = false;
        let mut unreadable: Vec<PathBuf> = Vec::new();
        let mut visited: Vec<PathBuf> = Vec::new();
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
        let mut opened = 0usize;

        while let Some((dir, depth)) = stack.pop() {
            opened += 1;
            if opened.is_multiple_of(BUDGET_CHECK_INTERVAL)
                && started.elapsed() >= self.policy.walk_budget
            {
                truncated = true;
                break;
            }
            if !seen.insert(dir.clone()) {
                continue;
            }
            visited.push(dir.clone());

            let mtime = std::fs::symlink_metadata(&dir)
                .ok()
                .and_then(|md| md.modified().ok());

            if self.can_reuse(&dir, mtime) {
                self.stats.directories_revalidated += 1;
                truncated |= self.push_children_of(&dir, depth, &mut stack);
                continue;
            }

            let Ok(entries) = std::fs::read_dir(&dir) else {
                // #6926: a refused subtree is disclosed, never fatal, and never
                // cached — the next refresh retries it.
                unreadable.push(dir.clone());
                self.nodes.remove(&dir);
                continue;
            };
            self.stats.directories_read += 1;
            let (own_bytes, children) = sum_entries(entries);
            self.nodes.insert(
                dir.clone(),
                DirNode {
                    mtime,
                    own_bytes,
                    children,
                    read_at: Instant::now(),
                },
            );
            truncated |= self.push_children_of(&dir, depth, &mut stack);
        }

        // A truncated walk never reached part of the tree, so its unvisited
        // nodes are stale-but-good rather than gone; sweeping them would throw
        // away exactly the cache the next refresh needs.
        if !truncated {
            self.nodes
                .retain(|path, _| !path.starts_with(root) || seen.contains(path));
        }

        RootRecord {
            bytes: self.fold_totals(&visited, root),
            measured_at: SystemTime::now(),
            read_at: Instant::now(),
            truncated,
            unreadable,
        }
    }

    /// Whether `dir`'s cached listing may be reused on this refresh.
    ///
    /// What: both clocks must agree — the mtime must be known and unchanged
    /// (nothing was created, deleted, or renamed in this directory) AND the
    /// node must be younger than [`IndexPolicy::node_max_age`] (the backstop
    /// for a file that grew in place, which moves no directory mtime).
    /// Test: `the_node_backstop_re_reads_a_file_that_grew_in_place`,
    /// `the_node_backstop_bound_is_exact` (both sides of `node_max_age`).
    fn can_reuse(&self, dir: &Path, mtime: Option<SystemTime>) -> bool {
        self.nodes.get(dir).is_some_and(|node| {
            node.mtime.is_some()
                && node.mtime == mtime
                && node.read_at.elapsed() < self.policy.node_max_age
        })
    }

    /// Queue `dir`'s cached children, unless the depth cap stops us.
    ///
    /// Returns whether the cap truncated the walk here.
    fn push_children_of(
        &self,
        dir: &Path,
        depth: usize,
        stack: &mut Vec<(PathBuf, usize)>,
    ) -> bool {
        let Some(node) = self.nodes.get(dir) else {
            return false;
        };
        if depth >= self.policy.max_depth {
            return !node.children.is_empty();
        }
        stack.extend(node.children.iter().map(|c| (c.clone(), depth + 1)));
        false
    }

    /// Sum subtree totals bottom-up and return the root's.
    ///
    /// What: `visited` is DFS pre-order, so a parent always precedes its
    /// children; walking it in reverse therefore has every child's subtotal in
    /// hand before its parent is folded. A directory with no node (one the OS
    /// refused) contributes zero, which is what [`DirSize::unreadable`]
    /// discloses.
    fn fold_totals(&self, visited: &[PathBuf], root: &Path) -> u64 {
        let mut subtotal: HashMap<&Path, u64> = HashMap::with_capacity(visited.len());
        for path in visited.iter().rev() {
            let total = self.nodes.get(path.as_path()).map_or(0, |node| {
                node.children
                    .iter()
                    .filter_map(|child| subtotal.get(child.as_path()))
                    .fold(node.own_bytes, |acc, bytes| acc.saturating_add(*bytes))
            });
            subtotal.insert(path.as_path(), total);
        }
        subtotal.get(root).copied().unwrap_or(0)
    }
}

impl RootRecord {
    fn to_dir_size(&self, path: PathBuf, from_cache: bool) -> DirSize {
        DirSize {
            path,
            bytes: self.bytes,
            measured_at: self.measured_at,
            truncated: self.truncated,
            unreadable: self.unreadable.clone(),
            from_cache,
        }
    }
}

/// The form the forbidden-root comparison is made in.
///
/// Why (#6926): `Path::starts_with` compares components literally, so
/// `$HOME/x/../..` reads as unrelated to `$HOME` while naming its parent. A
/// guard that compares raw spellings therefore lets a `..`-laced path walk the
/// exact tree it was configured to refuse. Both the candidate and the forbidden
/// list are resolved through here so the comparison is between directories, not
/// between strings.
/// What: `canonicalize` where it succeeds — it resolves `.`, `..`, and symlinked
/// ancestors, which is strictly the stronger guard. When it cannot (a missing or
/// unreadable ancestor), falls back to [`lexical_normalize`], which resolves
/// `.` and `..` without touching the filesystem. The fallback is weaker against
/// a symlinked ancestor; it is a floor, not the intended path.
/// Test: `a_dot_dot_path_into_a_forbidden_root_is_refused`.
fn resolve_for_guard(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| lexical_normalize(path))
}

/// Resolve `.` and `..` textually, without a syscall.
///
/// Why: [`resolve_for_guard`] cannot leave a path unresolved when
/// `canonicalize` fails — an unresolved `..` is exactly the traversal the guard
/// exists to catch. This is the floor it falls back to, so it has to fold `..`
/// on its own rather than returning the path untouched.
/// What: `..` pops the previous component, `.` is dropped, everything else is
/// kept. Popping past the root is a no-op, so `/a/../..` is `/` rather than an
/// empty path or an error. A leading `..` on a RELATIVE path pops nothing and is
/// discarded, which is one reason this is only the fallback — every root the
/// guard sees in practice is absolute, and `canonicalize` handles the rest.
/// Test: `lexical_normalize_folds_dot_and_dot_dot`.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// One directory's direct file bytes and its child directories.
///
/// What: entry types come from `DirEntry::file_type`, which does not traverse a
/// symlink, and a symlink is skipped outright — neither counted as a file nor
/// descended into as a directory. That is what keeps a link pointing out of a
/// worktree from inflating the figure or looping.
/// Test: `a_symlink_is_neither_followed_nor_counted`.
fn sum_entries(entries: std::fs::ReadDir) -> (u64, Vec<PathBuf>) {
    let mut own_bytes = 0u64;
    let mut children = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            children.push(entry.path());
        } else if file_type.is_file()
            && let Ok(md) = entry.metadata()
        {
            own_bytes = own_bytes.saturating_add(md.len());
        }
    }
    (own_bytes, children)
}

#[cfg(test)]
#[path = "size_index_tests.rs"]
mod size_index_tests;
