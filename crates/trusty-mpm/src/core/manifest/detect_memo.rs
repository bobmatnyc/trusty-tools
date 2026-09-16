//! The per-operation memo for bundled-manifest stack detection (#7806).
//!
//! Why: `ManifestSources::resolve` runs `super::framework::framework_agent_scope`
//! eagerly, and one launch resolves the manifest from five-plus call sites
//! (`session_launch`, `session_assets`, `sync_assets`, `mcp_session_env`,
//! `project_skill_tier`) plus `stack_profile`. Each one repeated the FULL
//! nested-directory walk and re-read every manifest it found — measured at
//! 0.3–0.6s per walk on this repository, paid five times for an answer that
//! cannot change between the first call and the last. Detection is a pure
//! function of the tree, so the second walk is waste.
//!
//! What: a process-wide map from project directory to the detection that
//! directory produced, entered through [`memoized`] so the compute path and the
//! store can never drift apart. A hit performs NO filesystem access at all: no
//! directory walk, no read, not even a `stat` — the key is the caller's path as
//! given, never canonicalized.
//!
//! **What invalidates an entry: an explicit [`invalidate_stack_detection`]
//! call, and nothing else.** No mtime, no content hash, no expiry — a stamp
//! cheap enough to check on every hit cannot see a manifest appearing in a
//! directory the walk has not visited, so it would trade a whole class of
//! silent staleness for an illusion of freshness. Instead each top-level
//! OPERATION opens a fresh scope for the project it is about to act on:
//! `session_launch::prepare_session_inner` and
//! `session_launch::sync_assets::sync_session_assets` invalidate at entry, so a
//! long-lived daemon re-detects once per launch or sync rather than once per
//! process, and every call inside that operation shares one walk. An entry
//! point added later gets the same one-line call; until it does, it reads an
//! answer at most one operation old — never worse than an unbounded
//! process-lifetime cache, and never staler than the last operation on that
//! project.
//!
//! Global mutable state is what `docs/reference/common-pitfalls.md` warns
//! against, and this is a deliberate exception: the detection call sites are
//! independent free functions reached from five unrelated call chains, none of
//! which threads a context object the memo could live on, and inventing one
//! would mean plumbing a parameter through every one of them to cache a value
//! that is a pure function of a path.
//! Test: `stack_detection_is_memoized_per_project`,
//! `invalidating_picks_up_a_changed_tree`,
//! `memo_is_keyed_by_project_dir`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

use super::framework::StackDetection;
use super::schema::AgentSet;

/// One project's detection, computed in a single probe pass.
///
/// Why: the deploy path wants the [`AgentSet`] and the prompt path wants the
/// [`StackDetection`], and they are two views of ONE marker evaluation. Caching
/// them together is what keeps `framework_agent_scope` and
/// `detected_stack_engineers` from walking the tree once each.
/// What: the composed framework-tier selection and the detected stack, from the
/// same `MarkerProbe`.
/// Test: `stack_detection_is_memoized_per_project`.
pub(crate) struct Detection {
    /// The framework-tier agent selection.
    pub(crate) scope: AgentSet,
    /// The detected stack engineers and the two scan-bound flags.
    pub(crate) stack: StackDetection,
}

/// The memo, plus the per-key compute count the tests assert on.
#[derive(Default)]
struct Memo {
    /// Live entries. An absent key means "not detected yet, or invalidated".
    entries: HashMap<PathBuf, Arc<Detection>>,
    /// How many times each key has actually been computed. Never cleared by
    /// [`invalidate_stack_detection`], so a test can count walks per project
    /// without racing another test's key.
    #[cfg(test)]
    computes: HashMap<PathBuf, usize>,
}

static MEMO: LazyLock<Mutex<Memo>> = LazyLock::new(|| Mutex::new(Memo::default()));

/// The memo guard, recovering from a poisoned lock rather than panicking.
///
/// Why: a panic in one detection must not turn every later launch into a second
/// panic — the memo holds plain data, so the worst a poisoned lock can carry is
/// a missing entry, which is the state a fresh process is in anyway.
fn memo() -> MutexGuard<'static, Memo> {
    MEMO.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Return `project_dir`'s detection, computing it only on a miss.
///
/// Why: the single seam #7806 asks for — every caller goes through here, so
/// "one walk per operation" is a property of this function rather than a rule
/// each call site has to remember.
/// What: an [`Arc`] of the cached [`Detection`] when one is live; otherwise
/// `compute` runs (OUTSIDE the lock, so a slow walk never blocks another
/// project's lookup) and its result is stored and returned. An `Err` is
/// returned to the caller and never cached, so a transient failure cannot be
/// memoized into a permanent one.
/// Test: `stack_detection_is_memoized_per_project`, `memo_is_keyed_by_project_dir`.
pub(crate) fn memoized<E>(
    project_dir: &Path,
    compute: impl FnOnce() -> Result<Detection, E>,
) -> Result<Arc<Detection>, E> {
    if let Some(hit) = memo().entries.get(project_dir) {
        return Ok(Arc::clone(hit));
    }
    let value = Arc::new(compute()?);
    let mut guard = memo();
    #[cfg(test)]
    {
        *guard.computes.entry(project_dir.to_path_buf()).or_default() += 1;
    }
    guard
        .entries
        .insert(project_dir.to_path_buf(), Arc::clone(&value));
    Ok(value)
}

/// Drop `project_dir`'s memoized detection so the next call re-walks the tree.
///
/// Why: the ONLY thing that invalidates the memo — see this module's doc for
/// why a cheap freshness stamp cannot do the job. A top-level operation calls
/// this for the project it is about to act on, which bounds staleness to one
/// operation while still collapsing that operation's five-plus resolutions into
/// one walk.
/// What: removes the entry, if any. Calling it for a project that was never
/// detected is a no-op.
/// Test: `invalidating_picks_up_a_changed_tree`.
pub fn invalidate_stack_detection(project_dir: &Path) {
    memo().entries.remove(project_dir);
}

/// How many times `project_dir`'s detection has actually been computed.
///
/// Why: the seam the #7806 regression tests count through — a memo hit performs
/// no filesystem access, so "did the walk run" is exactly "did this number
/// move". Keyed per project so tests sharing the binary cannot race.
/// Test: `stack_detection_is_memoized_per_project`.
#[cfg(test)]
pub(crate) fn computes_for(project_dir: &Path) -> usize {
    memo().computes.get(project_dir).copied().unwrap_or(0)
}
