//! Report-only worktree reconciliation on a GIT-DERIVED key (#4207 slice 3,
//! issue #4288).
//!
//! Why: worktree reconciliation had nothing to reconcile WITH. Measured on the
//! dogfood machine 2026-07-28: 118 registered worktrees across two disjoint git
//! registries (`.base` bare clone 60, sibling non-bare clone 58, intersection
//! ZERO), against 56 `sessions.json` records of which 31 carry a
//! `workspace_path` — and only THREE of the 118 worktrees have a matching
//! session record. Nothing in the store could say which registry a worktree
//! belonged to, so every consumer inferred it from the path.
//!
//! Path shape is provably the wrong discriminator: it misclassifies 34 of the
//! 118 (29%). Fourteen paths containing `/.base/` are registered in the SIBLING
//! registry; twenty paths NOT containing `/.base/` are registered in the BASE
//! one. Two sibling directories sharing a prefix land in different registries:
//!
//! ```text
//! .base/.worktrees/fix-3822-sessions-list-empty  -> common-dir <proj>/.git
//! .base/.worktrees/2eb72dca-de08-…               -> common-dir <proj>/.base
//! ```
//!
//! # The key
//!
//! [`WorktreeKey`] is the pair
//!
//! ```text
//! ( canonicalize(git -C <wt> rev-parse --path-format=absolute --git-common-dir),
//!   basename of <wt>'s admin dir, read from the `gitdir:` line of <wt>/.git )
//! ```
//!
//! asserting `git -C <wt> rev-parse --show-toplevel == canonicalize(<wt>)`.
//! This is git's OWN registry identity: derived entirely from git, dependent on
//! no path convention, and — decisively for #4269 — it does not contain `root`.
//! Every other anchor loses to something (measured, #4288):
//!
//! | Candidate | `mv proj` | GH rename | GH transfer | `remote remove origin` |
//! |---|---|---|---|---|
//! | `root` (which `project_index_id` used to advise) | **silently moves** | stable | stable | stable |
//! | origin URL | stable | **moves** | **moves** | **vanishes** |
//! | initial-commit SHA | stable | stable | stable | **not partitioning** |
//! | id file in the worktree | stable | stable | stable | **not git-derived** |
//! | common-dir + admin-dir name | **breaks LOUDLY** | stable | stable | stable |
//!
//! Named residuals, none of which this slice hides:
//!
//! 1. Moving the repository directory. `gitdir:` pointers are ABSOLUTE, so `mv`
//!    breaks them until `git worktree repair`. That failure is LOUD — git
//!    errors, the record goes `prunable`, `show-toplevel` stops matching — where
//!    a `root`-keyed scheme fails SILENTLY by re-deriving a fresh key and
//!    orphaning the old one. A loud break classifies [`ReconcileState::Unknown`]
//!    and is never reclaimed.
//! 2. macOS firmlinks / bind mounts: one tree, two common-dirs. Inherent to any
//!    path-based key; the same limitation is disclosed at
//!    `trusty_common::project_index_id`.
//! 3. A hand-copied admin directory (`cp -r`): two paths, one key. Caught by the
//!    `show-toplevel` assertion, classified `Unknown`.
//!
//! # Three states, because two cannot say "I could not observe this"
//!
//! `worktree_registry`'s rule — "an unanswerable probe is never a verdict" —
//! applies with more force here, because reconciliation reads paths nobody
//! provisioned. [`ReconcileState`] is therefore LIVE / ORPHANED / UNKNOWN, and
//! UNKNOWN DOMINATES: if the git-derived key does not resolve, no other signal
//! is allowed to promote the entry, because we have not established that the
//! path is a worktree at all. That is what keeps the second landmine safe —
//! `…/.worktrees/tm-trusty-tools-01` is a `workspace_path` in `sessions.json`,
//! has no `.git` file, and `rev-parse --show-toplevel` from inside it resolves
//! to the PARENT checkout. Any "workspace_path not in `git worktree list` =>
//! orphan => remove" rule deletes files inside a live checkout, invisibly.
//!
//! Every LIVE signal is a VETO on reclamation, never a vote for it. Liveness has
//! no single trustworthy source: session `2eb72dca-…` was measured RUNNING in
//! tmux pane `%981` while recorded `state: "stopped"`; "no tmux pane => orphan"
//! would mark 116 of 118 orphaned; mtime is useless (107 of 118 touched within
//! 7 days, none older than 30). The sentinel is the only signal with real
//! provenance and it covers 3 of 85 admitted candidates, two of those
//! zero-byte.
//!
//! # Scope: REPORT-ONLY. Zero new destructive code paths.
//!
//! Nothing in this module writes, deletes, or mutates anything — not
//! `sessions.json`, not a sentinel, not git. [`reconcile_worktrees`] is a pure
//! function of (registry scan, store snapshot, live cwds, clock).
//! [`ReconcileReport::proposed_adoptions`] NAMES the worktrees a later slice
//! would adopt, with the evidence for each inference, and writes none of them.
//! The reclaimer already exists, is already dry-run by default, and already
//! reclaims nothing; arming it is a later slice reviewed on its own merits.
//!
//! What: [`WorktreeKey`] + [`worktree_key`] (the key), [`ReconcileState`] /
//! [`WorktreeCategory`] / [`ReconciledWorktree`] (one classified entry),
//! [`ProposedAdoption`], [`ReconcileCounts`], [`ReconcileReport`], and
//! [`reconcile_worktrees`] (the whole inventory).
//! Test: `worktree_reconcile_tests`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::record::{ManagedSessionId, SessionRecord};
use super::worktree_ownership::{OWNERLESS_GRACE, SentinelOwner, read_sentinel_owner};
use super::worktree_registry::{Admission, ScannedWorktree, scan_registered_worktrees};
use super::worktree_safety::{git_command, inspect_dirt};

/// Git's own identity for a worktree's registry entry (#4288).
///
/// Why: see the module doc's anchor-comparison table — this is the only
/// candidate that survives a directory rename, a GitHub rename, a transfer, and
/// `git remote remove origin`, and the only one that breaks LOUDLY rather than
/// silently when it does break.
/// What: `common_dir` is the canonicalized shared git directory every worktree
/// of a repository points at (`<root>/.git` for a normal checkout, the bare
/// directory itself for a bare clone) — it is what PARTITIONS the two disjoint
/// registries. `admin_dir` is the basename of this worktree's own
/// `<common_dir>/worktrees/<name>` administrative directory, read from the
/// `gitdir:` line of the worktree's `.git` FILE — it is what IDENTIFIES the
/// entry within that registry. `None` means the `.git` entry is a directory,
/// i.e. a main checkout rather than a linked worktree.
/// Test: `reconcile_keys_on_the_git_registry_not_the_path` (baseline plus all
/// four mutations), `key_is_none_for_a_plain_directory_inside_a_checkout`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct WorktreeKey {
    /// Canonicalized `git rev-parse --git-common-dir` — the registry.
    pub common_dir: PathBuf,
    /// Basename of `<common_dir>/worktrees/<name>` — the entry in it.
    pub admin_dir: Option<String>,
}

/// Derive [`WorktreeKey`] for `candidate`, or `None` if it cannot be
/// established (#4288).
///
/// Why: a `None` here is the load-bearing safety result, not a degenerate case.
/// It is returned for the plain-subdirectory-inside-a-live-checkout shape
/// (landmine 2), for a hand-copied admin directory, and for a repository that
/// has been `mv`d without `git worktree repair` — and [`reconcile_worktrees`]
/// turns every `None` into [`ReconcileState::Unknown`], which is never
/// reclaimable.
/// What: canonicalizes `candidate`; asserts
/// `git rev-parse --path-format=absolute --show-toplevel` canonicalizes to the
/// SAME path (this is the assertion that rejects a subdirectory, whose
/// toplevel is its enclosing checkout); then reads `--git-common-dir` and the
/// `gitdir:` line of `<candidate>/.git`. Any failed probe yields `None` — never
/// a guessed key.
/// Test: `reconcile_keys_on_the_git_registry_not_the_path`,
/// `key_is_none_for_a_plain_directory_inside_a_checkout`.
pub fn worktree_key(candidate: &Path) -> Option<WorktreeKey> {
    let canonical = std::fs::canonicalize(candidate).ok()?;
    let toplevel = git_rev_parse_path(&canonical, "--show-toplevel")?;
    if std::fs::canonicalize(&toplevel).ok()? != canonical {
        return None;
    }
    let common_dir = git_rev_parse_path(&canonical, "--git-common-dir")?;
    Some(WorktreeKey {
        common_dir: std::fs::canonicalize(&common_dir).ok()?,
        admin_dir: admin_dir_name(&canonical),
    })
}

/// One absolute path from `git rev-parse`, or `None` on any failure.
///
/// Why: both halves of the key come from the same hardened invocation
/// (`worktree_safety::git_command` strips the environment variables that could
/// redirect git at another repository), and an empty stdout must read as "could
/// not observe", not as an empty path.
/// What: `git -C <dir> rev-parse --path-format=absolute <flag>`.
/// Test: covered by `reconcile_keys_on_the_git_registry_not_the_path` and
/// `key_is_none_for_a_plain_directory_inside_a_checkout`.
fn git_rev_parse_path(dir: &Path, flag: &str) -> Option<PathBuf> {
    let out = git_command(dir, &["rev-parse", "--path-format=absolute", flag])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

/// Basename of the worktree's administrative directory (#4288).
///
/// Why: `common_dir` alone identifies the REGISTRY, not the ENTRY — every
/// worktree of one repository shares it. The admin-dir name is git's own
/// per-entry handle, it is assigned at `git worktree add` time, and `git
/// worktree move` does NOT change it, which is what makes the key survive a
/// move.
/// What: reads `<worktree>/.git` as a file and returns the final component of
/// its `gitdir:` line. A main checkout's `.git` is a DIRECTORY, so the read
/// fails and this returns `None` — the correct answer, not an error.
/// Test: `reconcile_keys_on_the_git_registry_not_the_path` (mutation 1).
fn admin_dir_name(worktree: &Path) -> Option<String> {
    let content = std::fs::read_to_string(worktree.join(".git")).ok()?;
    let gitdir = content
        .lines()
        .find_map(|l| l.trim().strip_prefix("gitdir:"))?
        .trim();
    Path::new(gitdir)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// The three-state verdict for one reconciled worktree (#4288).
///
/// Why: two states cannot express "I could not observe this", and 82 of the 85
/// admitted candidates on the dogfood machine are exactly that. Collapsing them
/// into either of the other two would be a fabricated verdict about a directory
/// that may hold the only copy of somebody's work.
/// What: [`Live`](Self::Live) — some signal VETOES reclamation. [`Orphaned`](Self::Orphaned)
/// — every gate the existing reclaimer applies passes. [`Unknown`](Self::Unknown)
/// — everything else, including every unanswerable probe. Report, never act.
/// Test: `stopped_records_workspace_is_live_not_orphaned`,
/// `clean_ownerless_admitted_worktree_is_orphaned`,
/// `record_pointing_at_a_plain_directory_is_unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileState {
    /// A liveness signal vetoes reclamation.
    Live,
    /// Provably reclaimable under the gates the existing reclaimer applies.
    Orphaned,
    /// Not established. Reported, never acted on.
    Unknown,
}

/// A DESCRIPTIVE label for what a worktree appears to be (#4288).
///
/// Why: an operator reading a 118-row inventory needs to know at a glance which
/// rows are their own checkouts and which are agent scratch. But note what this
/// is NOT: it is derived partly from path shape, and path shape misclassifies
/// 29% of worktrees' REGISTRY membership. So this label is report text only —
/// it is never an input to [`ReconcileState`], never gates reclamation, and no
/// consumer may promote it to one. The classification uses git, the store, and
/// the sentinel; the category exists so a human can skim.
/// What: [`SessionOwned`](Self::SessionOwned) is the one EVIDENCE-based variant
/// (a store record or a sentinel names an owner); the rest are shape hints.
/// Test: `categories_are_descriptive_and_do_not_gate_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeCategory {
    /// A session record or an ownership sentinel names an owner. Evidence-based.
    SessionOwned,
    /// Lives under a `.claude/worktrees` agent-runtime directory.
    HarnessAgent,
    /// Lives under a temp root (`/tmp`, `/private/tmp`, `/private/var/folders`).
    TmpScratchpad,
    /// A repository's main checkout.
    ProjectCheckout,
    /// Registered, but outside the managed project — an operator's own tree.
    OperatorCheckout,
    /// None of the above.
    Unclassified,
}

/// One reconciled worktree: the union of what git, the store, and the sentinel
/// each say about one path (#4288).
///
/// Why: the report has to be actionable per ROW, so every field a reader would
/// otherwise have to go and derive by hand is carried here — above all the
/// `reason`, because a verdict without its reason is exactly the "false
/// reclaimable preview" #4288 names as a defect in its own right.
/// What: `admission` is `None` when git registers no such worktree at all (the
/// entry came only from a `SessionRecord.workspace_path`); `dirty` is populated
/// only for entries that reached the dirt gate (see [`reconcile_worktrees`]);
/// `contains_other_entry` marks a worktree with another inventory entry nested
/// inside it, which bars it from [`ReconcileReport::proposed_adoptions`].
/// Test: `report_unions_registry_and_session_records`.
#[derive(Debug, Clone, Serialize)]
pub struct ReconciledWorktree {
    /// Canonical path (raw when it would not canonicalize).
    pub path: PathBuf,
    /// Git's identity for this worktree, or `None` if not established.
    pub key: Option<WorktreeKey>,
    /// The three-state verdict.
    pub state: ReconcileState,
    /// Why that verdict, in one operator-facing line.
    pub reason: String,
    /// Descriptive label. Never an input to `state`.
    pub category: WorktreeCategory,
    /// Git's admission verdict, or `None` when git registers no such worktree.
    pub admission: Option<Admission>,
    /// Sessions whose `workspace_path` is this path, whatever their state.
    pub sessions: Vec<ManagedSessionId>,
    /// Owner named by the on-disk ownership sentinel, if it parsed.
    pub sentinel_owner: Option<ManagedSessionId>,
    /// Checked-out branch, when git reported one.
    pub branch: Option<String>,
    /// Dirt reason, when the dirt gate ran and reported unsaved work.
    pub dirty: Option<String>,
    /// Another inventory entry is nested inside this one.
    pub contains_other_entry: bool,
}

/// A worktree a later slice WOULD adopt, named with its evidence (#4288 item 3).
///
/// Why: the open decision on #4288 resolved to (b) — name the adoption set,
/// write nothing. Naming it is what makes the later backfill slice reviewable
/// against a list an operator has already seen, instead of a set of writes
/// nobody enumerated first.
/// What: the path, the inferred owning session, and the sentence explaining
/// which observation produced that inference. Nothing here is persisted.
/// Test: `proposed_adoptions_name_owner_and_evidence`,
/// `proposed_adoptions_refuse_a_worktree_containing_another_entry`.
#[derive(Debug, Clone, Serialize)]
pub struct ProposedAdoption {
    /// The worktree that would be adopted.
    pub path: PathBuf,
    /// The session that would be recorded as its owner.
    pub inferred_owner: ManagedSessionId,
    /// The observation behind that inference.
    pub evidence: String,
}

/// Row counts for the reconciled inventory (#4288).
///
/// Why: the headline an operator reads first, and the numbers a test can assert
/// on without depending on iteration order.
/// What: `total` counts every row; `admitted`/`excluded` split them by whether
/// git's registry scan made them reclaim candidates; `record_only` counts rows
/// that exist solely because a `SessionRecord` points at them.
/// Test: `report_unions_registry_and_session_records`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ReconcileCounts {
    /// Every row in the inventory.
    pub total: usize,
    /// Rows whose state is [`ReconcileState::Live`].
    pub live: usize,
    /// Rows whose state is [`ReconcileState::Orphaned`].
    pub orphaned: usize,
    /// Rows whose state is [`ReconcileState::Unknown`].
    pub unknown: usize,
    /// Rows git admitted as reclaim candidates.
    pub admitted: usize,
    /// Rows git registered but excluded, with a reason on each row.
    pub excluded: usize,
    /// Rows present only in `sessions.json`, registered by no git registry.
    pub record_only: usize,
}

/// The whole report (#4288).
///
/// Why: one value the HTTP route and the CLI both render, so the two surfaces
/// cannot drift into disagreeing about what was found.
/// What: `entries` sorted DEEPEST-FIRST so a nested worktree always precedes
/// its container; `proposed_adoptions`; `counts`.
/// Test: `entries_are_ordered_deepest_first`.
#[derive(Debug, Clone, Serialize)]
pub struct ReconcileReport {
    /// Every reconciled worktree, deepest path first.
    pub entries: Vec<ReconciledWorktree>,
    /// Names only — this slice writes nothing.
    pub proposed_adoptions: Vec<ProposedAdoption>,
    /// Headline counts.
    pub counts: ReconcileCounts,
}

/// Reconcile every git-registered worktree under `repos_root` against
/// `records`, report-only (#4288).
///
/// Why: this is slice 3. It answers, for the first time, "which of these 118
/// directories does this daemon actually know about?" — on a key git itself
/// maintains, so the answer does not silently change when a project directory
/// is renamed.
/// What: unions the full registry scan
/// ([`super::worktree_registry::scan_registered_worktrees`], which reports the
/// EXCLUDED set too) with every `SessionRecord.workspace_path`, derives
/// [`worktree_key`] per path, and classifies each into [`ReconcileState`].
/// `live_cwds` is the set of observed live working directories (tmux pane
/// `pane_current_path` values) — each is a veto. `now` is injected so the
/// [`OWNERLESS_GRACE`] window is testable.
///
/// The dirt probe ([`inspect_dirt`]) is deliberately run ONLY on entries that
/// have already passed the sentinel + ownerless gates: it walks nested
/// repositories and would otherwise cost minutes across 118 worktrees, and it
/// can only ever move an entry from ORPHANED to UNKNOWN, never the reverse.
///
/// PURE AND READ-ONLY: this function writes nothing. Its worst possible failure
/// is an inaccurate report.
/// Test: `report_unions_registry_and_session_records`,
/// `stopped_records_workspace_is_live_not_orphaned`,
/// `record_pointing_at_a_plain_directory_is_unknown`,
/// `clean_ownerless_admitted_worktree_is_orphaned`,
/// `reconcile_keys_on_the_git_registry_not_the_path`.
pub fn reconcile_worktrees(
    repos_root: &Path,
    records: &[SessionRecord],
    live_cwds: &[PathBuf],
    now: DateTime<Utc>,
) -> ReconcileReport {
    let scanned: BTreeMap<PathBuf, ScannedWorktree> = scan_registered_worktrees(repos_root)
        .into_iter()
        .map(|s| (s.path.clone(), s))
        .collect();
    let mut by_workspace: BTreeMap<PathBuf, Vec<&SessionRecord>> = BTreeMap::new();
    for r in records {
        if let Some(ws) = r.workspace_path.as_deref() {
            let key = std::fs::canonicalize(ws).unwrap_or_else(|_| ws.to_path_buf());
            by_workspace.entry(key).or_default().push(r);
        }
    }
    let universe: BTreeSet<PathBuf> = scanned.keys().chain(by_workspace.keys()).cloned().collect();

    let mut entries: Vec<ReconciledWorktree> = universe
        .iter()
        .map(|path| {
            classify(
                path,
                scanned.get(path),
                by_workspace.get(path).map_or(&[][..], Vec::as_slice),
                records,
                live_cwds,
                now,
            )
        })
        .collect();

    // Deepest first (#4288 item 4): a nested worktree must precede the worktree
    // that contains it, so a reader — and any later reclaimer — sees the child
    // before the parent whose removal would take the child's directory with it.
    entries.sort_by(|a, b| {
        b.path
            .components()
            .count()
            .cmp(&a.path.components().count())
            .then_with(|| a.path.cmp(&b.path))
    });
    for entry in &mut entries {
        entry.contains_other_entry = universe
            .iter()
            .any(|other| other != &entry.path && other.starts_with(&entry.path));
    }

    let proposed_adoptions = propose_adoptions(&entries, records);
    let counts = count(&entries);
    ReconcileReport {
        entries,
        proposed_adoptions,
        counts,
    }
}

/// Tally [`ReconcileCounts`] over the classified rows.
///
/// Why: keeps [`reconcile_worktrees`] readable and makes the counting rule one
/// thing rather than seven ad-hoc accumulators.
/// What: one pass over `entries`.
/// Test: `report_unions_registry_and_session_records`.
fn count(entries: &[ReconciledWorktree]) -> ReconcileCounts {
    let mut c = ReconcileCounts {
        total: entries.len(),
        ..ReconcileCounts::default()
    };
    for e in entries {
        match e.state {
            ReconcileState::Live => c.live += 1,
            ReconcileState::Orphaned => c.orphaned += 1,
            ReconcileState::Unknown => c.unknown += 1,
        }
        match e.admission {
            Some(Admission::Admitted) => c.admitted += 1,
            Some(_) => c.excluded += 1,
            None => c.record_only += 1,
        }
    }
    c
}

/// Classify one path into a [`ReconciledWorktree`] (#4288).
///
/// Why: the ORDER of the tests below is the safety property, and it only reads
/// as deliberate in one place. UNKNOWN dominates — an unresolved git key stops
/// classification dead, before any liveness signal can promote the row, because
/// a path we cannot prove is a worktree is a path we know nothing about.
/// Landmine 2 (`workspace_path` pointing at an ordinary subdirectory of a live
/// checkout) is exactly that row, and it must never be reported reclaimable.
/// What: key first; then the LIVE vetoes (a store record of ANY state, a live
/// cwd at or under the path, a sentinel owner that is not provably ownerless);
/// then ORPHANED only for an ADMITTED entry whose sentinel names a provably
/// ownerless owner and whose tree is clean; else UNKNOWN with the reason.
/// Test: `stopped_records_workspace_is_live_not_orphaned`,
/// `record_pointing_at_a_plain_directory_is_unknown`,
/// `clean_ownerless_admitted_worktree_is_orphaned`.
fn classify(
    path: &Path,
    scanned: Option<&ScannedWorktree>,
    owners: &[&SessionRecord],
    records: &[SessionRecord],
    live_cwds: &[PathBuf],
    now: DateTime<Utc>,
) -> ReconciledWorktree {
    let key = worktree_key(path);
    let sentinel = read_sentinel_owner(path);
    let sentinel_owner = match sentinel {
        SentinelOwner::Known(owner, _) => Some(owner),
        SentinelOwner::Unknown => None,
    };
    let mut entry = ReconciledWorktree {
        path: path.to_path_buf(),
        key: key.clone(),
        state: ReconcileState::Unknown,
        reason: String::new(),
        category: categorize(
            path,
            scanned,
            !owners.is_empty() || sentinel_owner.is_some(),
        ),
        admission: scanned.map(|s| s.admission),
        sessions: owners.iter().map(|r| r.id).collect(),
        sentinel_owner,
        branch: scanned.and_then(|s| s.branch.clone()),
        dirty: None,
        contains_other_entry: false,
    };

    // 1. UNKNOWN dominates: no git identity, no verdict.
    if key.is_none() {
        entry.reason = match scanned {
            Some(_) => "no git-derived key: `rev-parse --show-toplevel` disagrees this path is \
                        its own worktree root, or git could not answer — repair the registry \
                        (`git worktree repair`) before trusting any verdict here"
                .to_string(),
            None => "no git-derived key AND no git registry entry: this path is not a worktree \
                     (it may be an ordinary directory inside a live checkout) — never treat it \
                     as an orphan"
                .to_string(),
        };
        return entry;
    }

    // 2. LIVE vetoes. Each is a veto on reclamation, never a vote for it.
    if let Some(r) = owners.first() {
        entry.reason = format!(
            "live: session {} records this as its workspace_path (recorded state {:?}, which is \
             bookkeeping and NOT a liveness signal — the veto holds in every state)",
            r.id, r.state
        );
        entry.state = ReconcileState::Live;
        return entry;
    }
    if let Some(cwd) = live_cwds.iter().find(|c| c.starts_with(path)) {
        entry.reason = format!(
            "live: an observed working directory sits at or under it ({})",
            cwd.display()
        );
        entry.state = ReconcileState::Live;
        return entry;
    }
    if let SentinelOwner::Known(owner, created_at) = sentinel
        && !provably_ownerless(owner, created_at, records, now)
    {
        entry.reason = format!(
            "live: ownership sentinel names {owner}, which is neither terminal nor aged out"
        );
        entry.state = ReconcileState::Live;
        return entry;
    }

    // 3. ORPHANED, only for an admitted entry that clears every existing gate.
    if entry.admission != Some(Admission::Admitted) {
        entry.reason = scanned.map_or_else(
            || "unknown: a session record points here but no git registry lists it".to_string(),
            |s| format!("unknown: {}", s.admission.reason()),
        );
        return entry;
    }
    let SentinelOwner::Known(owner, _) = sentinel else {
        entry.reason = "unknown: no ownership sentinel (absent, empty, or unparsable) — never \
                        auto-reclaimable, this is the 82-of-85 case"
            .to_string();
        return entry;
    };
    if let Some(dirt) = inspect_dirt(path) {
        entry.reason = format!("unknown: holds unsaved work — {}", dirt.reason);
        entry.dirty = Some(dirt.reason);
        return entry;
    }
    entry.reason = format!(
        "orphaned: sentinel owner {owner} is provably ownerless, git admitted the path, and the \
         tree is clean"
    );
    entry.state = ReconcileState::Orphaned;
    entry
}

/// Is `owner` provably ownerless, against a store SNAPSHOT (#4288)?
///
/// Why: `SessionManager::resolve_ownerless_with_grace` is the authority, but it
/// is `async` and reads the live store; reconciliation classifies against one
/// consistent snapshot so every row in a report agrees with every other. The
/// RULE is copied exactly — divergence here would mean the report disagrees
/// with what the reclaimer would actually do, which is the false-preview defect.
/// What: a found record is ownerless iff its state is terminal; an absent
/// record is ownerless only once the sentinel is older than [`OWNERLESS_GRACE`]
/// (the sentinel is written BEFORE the record is persisted, so a young absent
/// owner is a creation race, not a deletion).
/// Test: `young_absent_sentinel_owner_is_live_not_orphaned`.
fn provably_ownerless(
    owner: ManagedSessionId,
    sentinel_created_at: DateTime<Utc>,
    records: &[SessionRecord],
    now: DateTime<Utc>,
) -> bool {
    match records.iter().find(|r| r.id == owner) {
        Some(record) => record.state.is_terminal(),
        None => now - sentinel_created_at > OWNERLESS_GRACE,
    }
}

/// The DESCRIPTIVE [`WorktreeCategory`] for one path.
///
/// Why/What: see [`WorktreeCategory`] — report text only, never an input to
/// [`ReconcileState`]. Evidence (a record or a sentinel) outranks every shape
/// hint, so `SessionOwned` is tested first.
/// Test: `categories_are_descriptive_and_do_not_gate_state`.
fn categorize(path: &Path, scanned: Option<&ScannedWorktree>, has_owner: bool) -> WorktreeCategory {
    if has_owner {
        return WorktreeCategory::SessionOwned;
    }
    let admission = scanned.map(|s| s.admission);
    if admission == Some(Admission::MainCheckout) {
        return WorktreeCategory::ProjectCheckout;
    }
    let is_agent_dir = path
        .components()
        .zip(path.components().skip(1))
        .any(|(a, b)| a.as_os_str() == ".claude" && b.as_os_str() == "worktrees");
    if is_agent_dir {
        return WorktreeCategory::HarnessAgent;
    }
    // "Temp scratchpad" is a claim about a worktree parked OUTSIDE any managed
    // project. Applying it by prefix alone would mislabel every worktree in a
    // repos root that itself lives under a temp directory — which is exactly the
    // shape every test fixture has, and a shape a user can trivially create.
    let inside_project = matches!(
        admission,
        Some(
            Admission::Admitted
                | Admission::Bare
                | Admission::Prunable
                | Admission::Locked
                | Admission::Unresolvable
        )
    );
    let text = path.to_string_lossy();
    let temp_rooted = text.starts_with("/tmp/")
        || text.starts_with("/private/tmp/")
        || text.starts_with("/var/folders/")
        || text.starts_with("/private/var/folders/");
    if !inside_project && temp_rooted {
        return WorktreeCategory::TmpScratchpad;
    }
    match admission {
        Some(Admission::OutsideProject | Admission::OutsideReposRoot) => {
            WorktreeCategory::OperatorCheckout
        }
        _ => WorktreeCategory::Unclassified,
    }
}

/// Name the worktrees a later slice would adopt, with the evidence (#4288 item 3).
///
/// Why: adoption is the write this slice deliberately does not perform. Listing
/// it is what lets the write be reviewed before it exists.
/// What: proposes only for ADMITTED entries with a resolved git key that do NOT
/// already carry a sentinel owner, and NEVER for a worktree containing another
/// inventory entry (#4288 item 4 — proposing a container is how a parent
/// removal takes its children). Evidence sources, strongest first: a
/// `SessionRecord.workspace_path`; a `SessionRecord.branch` equal to the
/// worktree's checked-out branch. Writes nothing.
/// Test: `proposed_adoptions_name_owner_and_evidence`,
/// `proposed_adoptions_refuse_a_worktree_containing_another_entry`.
fn propose_adoptions(
    entries: &[ReconciledWorktree],
    records: &[SessionRecord],
) -> Vec<ProposedAdoption> {
    let mut out = Vec::new();
    for e in entries {
        if e.admission != Some(Admission::Admitted)
            || e.key.is_none()
            || e.sentinel_owner.is_some()
            || e.contains_other_entry
        {
            continue;
        }
        if let Some(id) = e.sessions.first() {
            out.push(ProposedAdoption {
                path: e.path.clone(),
                inferred_owner: *id,
                evidence: format!(
                    "sessions.json: session {id} records this exact path as its workspace_path"
                ),
            });
            continue;
        }
        if let Some(branch) = e.branch.as_deref()
            && let Some(r) = records
                .iter()
                .find(|r| r.branch.as_deref() == Some(branch) && r.workspace_path.is_none())
        {
            out.push(ProposedAdoption {
                path: e.path.clone(),
                inferred_owner: r.id,
                evidence: format!(
                    "branch match: session {} records branch `{branch}` and has no workspace_path \
                     of its own",
                    r.id
                ),
            });
        }
    }
    out
}

impl super::manager::SessionManager {
    /// Gather the live inputs and produce the reconciled inventory (#4288).
    ///
    /// Why: [`reconcile_worktrees`] is pure so it can be tested against real
    /// git without a daemon; this is the thin layer that fetches the store
    /// snapshot and the observed live working directories. Keeping the git and
    /// filesystem work on a blocking thread matters — the scan runs `git` once
    /// per anchor and `rev-parse` twice per worktree, which is not something to
    /// do on the async runtime.
    /// What: snapshots every record, collects `pane_current_path` for each
    /// record whose tmux session is alive, and runs the reconcile on a blocking
    /// thread. Reads only; writes nothing anywhere.
    ///
    /// KNOWN INCOMPLETENESS, stated rather than hidden: the live-cwd set covers
    /// panes of MANAGED sessions only. A pane in an unmanaged tmux session, and
    /// a bare process `cwd`, are both signals #4288 names and neither is
    /// observed here. A missed LIVE veto cannot by itself make anything
    /// reclaimable — [`ReconcileState::Orphaned`] additionally requires a
    /// sentinel naming a provably-gone owner AND a clean tree — but it does
    /// mean a row can read `Unknown` where a richer observer would read `Live`.
    /// That is another reason this slice acts on nothing.
    /// Test: `reconcile_inventory_reports_without_mutating_anything`.
    pub(crate) async fn reconcile_worktree_inventory(
        &self,
        repos_root: &Path,
    ) -> Result<ReconcileReport, anyhow::Error> {
        let records = self.list().await;
        let mut live_cwds: Vec<PathBuf> = Vec::new();
        for r in &records {
            if self.tmux.session_exists(&r.tmux_name)
                && let Some(cwd) = self.tmux.get_pane_cwd(&r.tmux_name)
            {
                live_cwds.push(std::fs::canonicalize(&cwd).unwrap_or(cwd));
            }
        }
        let repos_root = repos_root.to_path_buf();
        tokio::task::spawn_blocking(move || {
            reconcile_worktrees(&repos_root, &records, &live_cwds, Utc::now())
        })
        .await
        .map_err(|e| anyhow::anyhow!("reconcile-worktrees: inventory scan panicked: {e}"))
    }
}

#[cfg(test)]
#[path = "worktree_reconcile_tests.rs"]
mod worktree_reconcile_tests;
