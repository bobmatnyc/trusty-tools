//! Git-native worktree enumeration and registry-root resolution (#4207 slice 1).
//!
//! Why: every worktree question this crate asks used to be answered by
//! GUESSING at the filesystem. `prune::find_orphaned_worktrees` walked five
//! hard-coded location shapes (`.worktrees/`, `.base/.worktrees/`,
//! `.claude/worktrees/`, `.base/.claude/worktrees/`, and
//! `.base/.worktrees/<id>/.claude/worktrees/`), and both
//! `worktree_safety::git_worktree_list_agrees` and
//! `decommission::remove_session_worktree` derived "which checkout owns this
//! worktree?" from the candidate's GRANDPARENT directory. Neither is a fact;
//! both are inferences that git can answer exactly, and both were wrong on
//! this machine on 2026-07-27: fourteen worktrees physically located inside
//! `<repo>/.base/.worktrees/` are registered to the PARENT repo `<repo>`, so
//! the grandparent rule asked `.base` about them, `.base` correctly said "not
//! mine", and the sweep skipped them conservatively — forever. They were not
//! merely un-reclaimed, they were STRUCTURALLY unreclaimable, and no amount of
//! adding a sixth location shape would have fixed it.
//!
//! Git already maintains the answer. `git worktree list --porcelain` is the
//! registry, and `git rev-parse --git-common-dir` names the checkout that owns
//! it. Deriving from those two commands makes location irrelevant: a worktree
//! is found because git registered it, not because it happened to be parked
//! under a directory name someone remembered to enumerate.
//!
//! What: [`RegisteredWorktree`] (one porcelain record),
//! [`parse_worktree_list`] (the pure parser), [`registry_root_for`] (replaces
//! grandparent-as-repo-root inference), [`list_registered_worktrees`] (what
//! git says about the repository owning a path),
//! [`scan_registered_worktrees`] (#4288 — EVERY registered worktree under the
//! repos root, each carrying the [`Admission`] verdict that says whether it is
//! a reclaim candidate and, when it is not, WHY), and
//! [`enumerate_registered_worktrees`] (the admitted subset — every registered
//! worktree living INSIDE a managed project under the repos root; see that
//! function's "positive assertion" section for the structural boundary that
//! bounds the sweep).
//!
//! # An unanswerable probe is never a verdict
//!
//! Every function here returns `Option`/`None` — never a `bool` — for the
//! "could not determine" case, so no caller can silently read a missing `git`
//! binary, a permissions error, or a stalled network mount as a fact about the
//! worktree. This is the same invariant PR #4202's round-1 fix broke by
//! collapsing every `canonicalize()` error kind into "destroyed"; on
//! 2026-07-21 a rule that loose would have told an operator to recreate ~70
//! fully intact worktrees. Callers decide what a `None` means in their own
//! direction of safety — [`enumerate_registered_worktrees`] treats it as "no
//! candidates from this anchor" (nothing is proposed for deletion), while
//! `git_worktree_list_agrees` keeps its long-standing best-effort `true`.
//!
//! Test: `worktree_registry_tests`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::worktree_safety::git_command;

/// The bare-clone checkout trusty-mpm provisions alongside a managed project.
///
/// Why: a managed project has at most TWO git checkouts that can own a worktree
/// registry — the project checkout itself and the `.base` clone
/// `provisioner::workspace::WorkspaceProvisioner` creates. Naming the segment
/// once keeps [`enumerate_registered_worktrees`] from repeating the literal.
/// This is NOT a worktree-location guess: it names a CHECKOUT to interrogate,
/// and git then reports where that checkout's worktrees actually live.
/// What: `".base"`.
/// Test: `enumerate_finds_worktree_registered_to_base_clone`.
const BASE_CLONE_DIRNAME: &str = ".base";

/// One worktree exactly as `git worktree list --porcelain` reports it (#4207).
///
/// Why: the porcelain format carries the four facts the reclaim path needs —
/// where the worktree is, whether it is the main checkout (never a reclaim
/// candidate), whether it is bare (no working tree to reclaim), and whether
/// git already considers it prunable (its directory is gone, so there is
/// nothing to delete). Modelling them as fields rather than re-grepping the
/// raw text at each call site keeps the parse in one place.
/// What: `path` is git's own absolute path for the worktree; `branch` is the
/// short branch name when the worktree is not detached; `is_main` marks the
/// FIRST record, which git always emits for the main worktree.
/// Test: `parse_worktree_list_reads_porcelain_records`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegisteredWorktree {
    /// Absolute path git reports for this worktree.
    pub path: PathBuf,
    /// Short branch name (`refs/heads/` stripped), or `None` when detached.
    pub branch: Option<String>,
    /// `true` for a bare repository record (no working tree).
    pub bare: bool,
    /// `true` when git reports the worktree's directory is missing.
    pub prunable: bool,
    /// `true` when git reports the worktree is LOCKED — an explicit operator
    /// "do not remove this", which `git worktree remove` refuses to override
    /// without `--force`.
    pub locked: bool,
    /// The lock's REASON when git reported one, else `None` (#6561).
    ///
    /// Why: a lock is not one thing. The operator's `git worktree lock` is a
    /// standing veto; the Claude Code harness's own agent-worktree isolation
    /// also locks, for the life of the dispatched agent, with a reason naming
    /// that agent. Discarding the reason made both indistinguishable, so a tree
    /// a live agent was working in was dropped at [`Admission::Locked`] and
    /// never reached the gate that would have SAID SO — which is half of the
    /// silent `0 of 0` #6561 reports.
    /// Test: `parse_worktree_list_reads_locked_with_and_without_reason`,
    /// `parse_worktree_list_keeps_the_harness_agent_lock_reason`.
    pub lock_reason: Option<String>,
    /// `true` for the first record — git always lists the main worktree first.
    pub is_main: bool,
}

/// Does `reason` name the Claude Code harness's own agent-worktree lock (#6561)?
///
/// Why: this is the ONE fact that separates the harness's agent-lifetime lock
/// from an operator's standing veto, and it has to be read from the reason
/// string because git records nothing else about who locked a worktree. Measured
/// on this machine (git 2.54.0, Claude Code agent isolation): every live agent's
/// tree carries `claude agent agent-<id> (pid <n> start <time>)`, and every
/// finished agent's tree carries no lock at all — the harness releases it when
/// the agent ends.
/// What: a case-insensitive `claude agent` prefix test on the trimmed reason.
/// Deliberately a PREFIX and not a parse: nothing downstream reads the agent id
/// or the pid out of this string, so recognising the shape is the whole job, and
/// a reason that merely mentions "claude agent" further along is not matched.
/// Test: `harness_agent_lock_reason_matches_the_harness_shape`,
/// `harness_agent_lock_reason_rejects_an_operator_lock`.
pub(crate) fn is_harness_agent_lock_reason(reason: &str) -> bool {
    reason
        .trim_start()
        .get(..12)
        .is_some_and(|head| head.eq_ignore_ascii_case("claude agent"))
}

/// What git can say, right now, about whether the harness holds `path` (#6561).
///
/// Why: the delegation registry is a `DashMap` rebuilt empty at every daemon
/// boot, so after a restart it answers "nothing claims it" for an agent that is
/// still working — [`super::worktree_ownership::AgentDelegationState::Unknown`],
/// which ADR-0045 forbids reading as "free". Git's lock does not have that
/// defect: it is a file under `.git/worktrees/<id>/locked`, written by the
/// harness when it dispatches an agent into the tree and removed when that agent
/// ends, and it survives a daemon restart because no daemon writes it. So it is
/// the durable second source that can answer the question the registry cannot.
/// What: three answers, and the third is the point. [`Held`](Self::Held) — git
/// reports a harness agent lock. [`Released`](Self::Released) — git lists the
/// worktree and reports no lock on it, which is POSITIVE evidence the harness
/// let go. [`Undeterminable`](Self::Undeterminable) — git could not be asked, or
/// no longer lists the path; never read as released.
/// Test: `harness_lock_state_reports_a_harness_locked_tree_held`,
/// `harness_lock_state_reports_an_unlocked_tree_released`,
/// `harness_lock_state_of_a_non_worktree_is_undeterminable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HarnessLockState {
    /// Git reports a lock whose reason names a harness agent.
    Held,
    /// Git lists this worktree and reports no lock on it at all.
    Released,
    /// Git could not be asked, or does not list this path.
    Undeterminable,
}

/// Ask git whether the harness still holds `path` (#6561).
///
/// Why: see [`HarnessLockState`]. Resolved from the candidate itself so the
/// answer comes from the repository that actually registered the worktree.
/// What: one `git -C <path> worktree list --porcelain`, matched on the
/// canonicalized path. An operator lock — a lock whose reason is not the
/// harness's — is reported [`Undeterminable`](HarnessLockState::Undeterminable)
/// rather than `Released`, because it is not evidence about the harness either
/// way and must never advance a destructive path.
/// Test: `harness_lock_state_reports_a_harness_locked_tree_held`,
/// `harness_lock_state_reports_an_unlocked_tree_released`,
/// `harness_lock_state_of_an_operator_locked_tree_is_undeterminable`,
/// `harness_lock_state_of_a_non_worktree_is_undeterminable`.
pub(crate) fn harness_lock_state(path: &Path) -> HarnessLockState {
    let Some(registered) = list_registered_worktrees(path) else {
        return HarnessLockState::Undeterminable;
    };
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let found = registered
        .into_iter()
        .find(|w| std::fs::canonicalize(&w.path).unwrap_or_else(|_| w.path.clone()) == canonical);
    let Some(record) = found else {
        return HarnessLockState::Undeterminable;
    };
    if !record.locked {
        return HarnessLockState::Released;
    }
    match record.lock_reason.as_deref() {
        Some(reason) if is_harness_agent_lock_reason(reason) => HarnessLockState::Held,
        _ => HarnessLockState::Undeterminable,
    }
}

/// Parse `git worktree list --porcelain` output into records (#4207).
///
/// Why: the parse is the one piece of this module that can be tested without
/// spawning git, and the porcelain format has enough shape (blank-line-
/// separated stanzas, valueless `bare`/`detached`/`prunable` attribute lines)
/// that an ad-hoc `strip_prefix("worktree ")` over all lines — what
/// `git_worktree_list_agrees` did before this change — silently ignores every
/// other fact in the record.
/// What: splits on blank lines; each stanza starts with `worktree <path>` and
/// may carry `HEAD <sha>`, `branch <ref>`, `bare`, `detached`, `locked`, or
/// `prunable` (the last two optionally with a reason). The first stanza is
/// flagged [`RegisteredWorktree::is_main`]. Unknown attribute lines are
/// ignored so a future git version cannot break the parse.
/// Test: `parse_worktree_list_reads_porcelain_records`,
/// `parse_worktree_list_marks_only_the_first_record_main`,
/// `parse_worktree_list_reads_locked_with_and_without_reason`.
pub(crate) fn parse_worktree_list(stdout: &str) -> Vec<RegisteredWorktree> {
    let mut out: Vec<RegisteredWorktree> = Vec::new();
    for line in stdout.lines() {
        let line = line.trim_end();
        if let Some(path) = line.strip_prefix("worktree ") {
            out.push(RegisteredWorktree {
                path: PathBuf::from(path),
                branch: None,
                bare: false,
                prunable: false,
                locked: false,
                lock_reason: None,
                is_main: out.is_empty(),
            });
            continue;
        }
        let Some(current) = out.last_mut() else {
            continue;
        };
        if let Some(branch) = line.strip_prefix("branch ") {
            current.branch = Some(
                branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch)
                    .to_string(),
            );
        } else if line == "bare" {
            current.bare = true;
        } else if line == "prunable" || line.starts_with("prunable ") {
            current.prunable = true;
        } else if line == "locked" {
            current.locked = true;
        } else if let Some(reason) = line.strip_prefix("locked ") {
            // #6561: the reason is kept, not discarded — it is the only thing
            // that tells the harness's agent-lifetime lock from an operator veto.
            current.locked = true;
            current.lock_reason = Some(reason.to_string());
        }
    }
    out
}

/// The checkout that owns the worktree registry `anchor` belongs to (#4207).
///
/// Why: this REPLACES grandparent-as-repo-root inference
/// (`path.parent().and_then(|p| p.parent())`), which was correct only for the
/// two shapes it was written against and wrong for every other one —
/// `<repo>/.claude/worktrees/<name>` resolved to `<repo>/.claude`, which is
/// not a repository at all, and the fourteen `.base`-resident worktrees
/// registered to the parent repo resolved to `.base`, which disowns them.
/// `git rev-parse --git-common-dir` answers the same question as a fact: it
/// names the SHARED git directory every worktree of a repository points at,
/// which is exactly the registry `git worktree list` reads.
/// What: runs `git -C <anchor> rev-parse --path-format=absolute
/// --git-common-dir`. A normal checkout answers `<root>/.git`, so the root is
/// its parent; a bare clone answers with the bare directory itself, which IS
/// the root. Returns `None` when git cannot be run, exits non-zero (including
/// the ordinary "not a git repository" case), or prints nothing usable — never
/// a guessed path.
/// Test: `registry_root_for_linked_worktree_is_the_owning_checkout`,
/// `registry_root_for_non_repo_is_none`.
pub(crate) fn registry_root_for(anchor: &Path) -> Option<PathBuf> {
    let out = git_command(
        anchor,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .output()
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let common_dir = PathBuf::from(raw.trim());
    if common_dir.as_os_str().is_empty() {
        return None;
    }
    // A non-bare repository's common dir is `<root>/.git`; a bare one IS the root.
    if common_dir.file_name().is_some_and(|n| n == ".git") {
        common_dir.parent().map(Path::to_path_buf)
    } else {
        Some(common_dir)
    }
}

/// Every worktree git registers for the repository that owns `anchor` (#4207).
///
/// Why: `git worktree list` is the registry. Asking it directly is what makes
/// worktree discovery independent of where a worktree is parked, and it is the
/// only source that can report a worktree registered to one checkout while
/// living inside another's directory tree — the exact state that made fourteen
/// worktrees on this machine unreclaimable.
/// What: runs `git -C <anchor> worktree list --porcelain` and parses the
/// result via [`parse_worktree_list`]. Git resolves the repository from
/// `anchor` itself, so `anchor` may be the checkout root, a linked worktree, or
/// any directory inside one. Returns `None` on a spawn failure or non-zero
/// exit — an unanswerable probe, never an empty list.
/// Test: `list_registered_worktrees_reports_a_real_worktree`,
/// `list_registered_worktrees_none_outside_a_repo`.
pub(crate) fn list_registered_worktrees(anchor: &Path) -> Option<Vec<RegisteredWorktree>> {
    let out = git_command(anchor, &["worktree", "list", "--porcelain"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_worktree_list(&String::from_utf8_lossy(&out.stdout)))
}

/// Every git-registered worktree living under `repos_root` (#4207 slice 1).
///
/// Why: this is the derive-not-walk replacement for
/// `prune::find_orphaned_worktrees`' five hard-coded location shapes. Those
/// shapes could only ever find worktrees someone had already thought to
/// enumerate, and each new agent runtime added a shape (#3649, #3971, and the
/// #3971 follow-up were all "we missed a location" fixes). Asking git removes
/// the category: a worktree is discovered because it is REGISTERED, wherever
/// it sits.
/// What: for each `<repos_root>/<owner>/<repo>/`, interrogates the two
/// checkouts trusty-mpm actually provisions — `<repo>` and
/// `<repo>/.base` — but only after confirming (via [`registry_root_for`]) that
/// the anchor IS that repository's root. That confirmation is load-bearing:
/// without it, `git -C <non-repo>` walks UP the filesystem and would report an
/// unrelated enclosing repository's worktrees. Records that are bare, main,
/// already prunable, or git-`locked` are dropped, as is any worktree failing
/// the [`scan_from_anchor`] containment rule. Results are canonicalized (so
/// they compare cleanly against the canonicalized active-session set) and
/// returned sorted and de-duplicated, since both anchors may report the same
/// worktree.
///
/// # Containment is a POSITIVE assertion, not a denylist (#4224 review, HIGH)
///
/// Enumerating from git's registry rather than from five path shapes widens
/// what can be SEEN, and the first cut of this module let anything git
/// registered anywhere under `repos_root` through — leaving the #3649 ownership
/// sentinel as the single structural boundary in front of an unattended
/// `remove_dir_all`. Replayed against the dogfood machine that admitted seven
/// paths the old walk never produced, FIVE of them the operator's own
/// long-lived checkouts (`<owner>/ft-follow-links`,
/// `<owner>/trusty-tools-main-build`, `<owner>/trusty-tools-worktrees/wt-*`,
/// `<owner>/gb-connect-card`). None was deletable — each lacks a sentinel — but
/// "one gate happens to hold" is not a boundary, and this repository has
/// already lost a bare clone to an over-broad removal (2026-07-21).
///
/// So a candidate must additionally be a STRICT DESCENDANT of the managed
/// project directory whose registry named it. That is an assertion the path IS
/// inside a tree trusty-mpm provisions and manages, which is an invariant of
/// the WRITE side: `provisioner::workspace` only ever creates worktrees beneath
/// `<repos_root>/<owner>/<repo>/` (in the checkout, in its `.base` clone, or in
/// an agent runtime's directory inside either). It is deliberately NOT a list
/// of blessed path shapes — any depth and any directory name inside the project
/// qualifies, so the "we forgot a location" bug class this PR deletes (#3649,
/// #3971) stays deleted. And it is not a denylist of known operator paths: an
/// operator checkout is excluded by its POSITION relative to the repository
/// that owns it, so a newly created one is excluded on the day it is created,
/// under any name.
///
/// Both operands come from git — the path from `git worktree list --porcelain`,
/// the project root from `git rev-parse --git-common-dir` — so this adds no
/// state that could drift out of sync with git's own registry. That matters:
/// worktrees present in one registry and absent from another is a failure this
/// repository has already had twice.
///
/// ## Why provenance is not available, and branch role does not substitute
///
/// The discriminator one WANTS is "was this provisioned for a writing agent, on
/// a workstream branch?". Git cannot answer the first half at all: nothing in
/// the porcelain record, and nothing in the `worktrees/<id>/` admin directory,
/// records WHO ran `git worktree add`. The only provenance fact that exists is
/// trusty-mpm's own #3649 sentinel, which is why the sentinel gate stays where
/// it is — this boundary is layered IN FRONT of it, not a second copy of it.
///
/// Branch role does not substitute, measured rather than assumed. Across the
/// 201 candidates on this machine the branch namespaces are
/// `fix/` (88), `feat/` (44), `docs/` (12), detached (10), `session/` (9),
/// `chore/` (9) — and the operator's own checkouts, the Claude-native
/// `agent-*` worktrees, and the genuinely tm-owned ones are ALL drawn from
/// `fix/` and `feat/`. Gating reclamation on a `session|ws|workstream`
/// namespace would admit 9 of 201 candidates and ZERO of the 3 sentinel-bearing
/// ones — reclamation would silently drop to nothing while still looking
/// healthy. A branch namespace is also just a name pattern, and an operator may
/// check out any branch in their own checkout.
///
/// Containment is therefore the strongest git-observable boundary available,
/// and it is count- and name-independent: the 1.3.0 direction (one worktree per
/// writing agent, more of them, shorter-lived) makes it MORE useful, not less,
/// because every agent worktree is provisioned inside the project tree no
/// matter how many there are or what they are called.
///
/// Measured on the same machine, the rule drops all five operator checkouts,
/// retains all 54 `.base`-resident worktrees (the class that was structurally
/// unreclaimable before this PR), and leaves the count of sentinel-bearing —
/// i.e. actually reclaimable — candidates unchanged at 3. The narrowing costs
/// no reclaim capability; it only removes reach the sweep never needed.
/// Test: `enumerate_finds_worktree_at_an_unwalked_location`,
/// `enumerate_finds_worktree_registered_to_base_clone`,
/// `enumerate_ignores_plain_directory_that_is_not_a_worktree`,
/// `enumerate_excludes_the_main_checkout`,
/// `enumerate_excludes_worktrees_outside_the_repos_root`,
/// `enumerate_excludes_a_sibling_checkout_of_the_same_repo`,
/// `enumerate_excludes_a_worktree_parked_beside_the_project`,
/// `enumerate_excludes_a_locked_worktree`.
pub(crate) fn enumerate_registered_worktrees(repos_root: &Path) -> Vec<PathBuf> {
    let admitted: BTreeSet<PathBuf> = scan_registered_worktrees(repos_root)
        .into_iter()
        .filter(|s| s.admission == Admission::Admitted)
        .map(|s| s.path)
        .collect();
    let mut out: Vec<PathBuf> = admitted.into_iter().collect();
    sort_deepest_first(&mut out);
    out
}

/// Order `paths` so a NESTED worktree always precedes the worktree that
/// contains it (#4288, in-scope item 4).
///
/// Why: the pre-#4288 order came straight out of a `BTreeSet<PathBuf>`, which
/// sorts lexicographically — so `…/parent` sorts BEFORE `…/parent/child`.
/// Nine registered worktrees on the dogfood machine are nested inside another
/// registered worktree, and removing a parent first takes its children's
/// directories with it: the child's own removal then finds nothing to remove
/// and git is left holding a dangling registry entry for a path that no longer
/// exists. Reversing the order makes each nested worktree removed by
/// `git worktree remove` (which de-registers it) before its parent is touched.
///
/// This changes ORDER only. It cannot admit a path the containment rules
/// already rejected, and it cannot cause a removal that would not otherwise
/// have happened — every element was already in the returned list.
/// What: sorts by path DEPTH descending (component count), breaking ties
/// lexicographically so the result stays deterministic. Depth-descending is
/// sufficient for the containment property: a descendant always has strictly
/// more components than its ancestor.
/// Test: `enumerate_orders_nested_worktree_before_its_parent`.
fn sort_deepest_first(paths: &mut [PathBuf]) {
    paths.sort_by(|a, b| {
        b.components()
            .count()
            .cmp(&a.components().count())
            .then_with(|| a.cmp(b))
    });
}

/// Why a registered worktree is, or is not, an orphan-reclaim CANDIDATE (#4288).
///
/// Why: `enumerate_registered_worktrees` returns only the admitted paths, so
/// everything it drops is invisible — and on the dogfood machine that is 33 of
/// 118 registered worktrees, 6 of which hold uncommitted work, including the
/// two landmines #4288 documents (a `workspace_path` that is an ordinary
/// subdirectory of a live checkout, and worktrees nested inside a live one).
/// Reconciliation cannot report what enumeration silently discards, so the
/// verdict is now a VALUE the scan carries rather than a `continue` statement.
/// What: [`Admitted`](Self::Admitted) is the reclaim-candidate set — identical
/// to what `enumerate_registered_worktrees` returned before #4288. Every other
/// variant names one exclusion, in the order the scan applies them.
/// Test: `scan_reports_the_excluded_set_with_reasons`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Admission {
    /// Inside a managed project, not bare/main/prunable/locked — a candidate.
    Admitted,
    /// A bare repository record: no working tree to reclaim.
    Bare,
    /// The repository's main checkout: never a candidate.
    MainCheckout,
    /// Git already considers the directory gone.
    Prunable,
    /// The operator ran `git worktree lock` — an explicit removal veto.
    Locked,
    /// The Claude Code harness holds its own agent-lifetime lock (#6561).
    ///
    /// Why a variant of its own rather than [`Locked`](Self::Locked): both are
    /// refusals, but only one of them is a fact the operator has to be told.
    /// An operator lock is the operator's own standing decision; a harness lock
    /// means a DISPATCHED AGENT is working in that tree right now, and folding
    /// it into `Locked` dropped the candidate at gate 1 of
    /// `worktree_reclaim::classify` — before the gate that reports a spared
    /// agent tree ever ran. So `--merged-prs` printed `0 of 0` over a store
    /// full of agent worktrees and named none of them.
    /// Test: `scan_separates_a_harness_agent_lock_from_an_operator_lock`,
    /// `classify_discloses_a_harness_locked_agent_tree_as_agent_owned`.
    HarnessAgentLock,
    /// The path could not be canonicalized, so nothing about it is comparable.
    Unresolvable,
    /// Registered, but not a strict descendant of the managed project.
    OutsideProject,
    /// Registered, but outside the managed repos root.
    OutsideReposRoot,
}

impl Admission {
    /// A short, operator-facing phrase naming this verdict (#4288).
    ///
    /// Why: the reconcile report prints one reason per entry, and building
    /// that string at each call site would let the wordings drift.
    /// What: a `&'static str` per variant.
    /// Test: `scan_reports_the_excluded_set_with_reasons`.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Admitted => "admitted: registered inside a managed project",
            Self::Bare => "excluded: bare repository record (no working tree)",
            Self::MainCheckout => "excluded: the repository's main checkout",
            Self::Prunable => "excluded: git reports the directory is already gone",
            Self::Locked => "excluded: git-locked by the operator",
            Self::HarnessAgentLock => {
                "excluded: the harness holds its agent-lifetime lock on this worktree — a \
                 dispatched agent is still working in it (#6561)"
            }
            Self::Unresolvable => "excluded: path could not be canonicalized",
            Self::OutsideProject => "excluded: not inside the managed project that registered it",
            Self::OutsideReposRoot => "excluded: outside the managed repos root",
        }
    }
}

/// One registered worktree plus everything the scan learned about it (#4288).
///
/// Why: reconciliation needs the registry that named a worktree (that is half
/// the git-derived key), the project bounding it, and the admission verdict —
/// none of which survive `enumerate_registered_worktrees`' `Vec<PathBuf>`.
/// What: `path` is canonical when it resolved (raw otherwise, so an
/// [`Admission::Unresolvable`] entry can still be REPORTED); `project` is the
/// managed project directory; `registry_root` is the checkout whose
/// `git worktree list` named it; `branch` and `admission` complete the record.
/// Test: `scan_reports_the_excluded_set_with_reasons`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScannedWorktree {
    /// Canonical worktree path (raw when canonicalization failed).
    pub path: PathBuf,
    /// The managed project directory `<repos_root>/<owner>/<repo>`.
    pub project: PathBuf,
    /// The checkout whose registry listed this worktree.
    pub registry_root: PathBuf,
    /// Short branch name, or `None` when detached.
    pub branch: Option<String>,
    /// Whether this is an orphan-reclaim candidate, and if not, why not.
    pub admission: Admission,
}

/// Every worktree BOTH managed anchors register under `repos_root`, admitted
/// or not (#4288).
///
/// Why: this is [`enumerate_registered_worktrees`] with the `continue`
/// statements turned into values, so reconciliation reports the excluded set
/// instead of inheriting enumeration's blind spot. Sharing one traversal is
/// the point — a second, parallel enumerator is exactly the drift that let the
/// pre-#4207 five-shape walk disagree with itself.
/// What: for each `<repos_root>/<owner>/<repo>/`, interrogates `<repo>` and
/// `<repo>/.base` (each only after [`registry_root_for`] confirms the anchor
/// IS that repository's root), and returns one [`ScannedWorktree`] per
/// porcelain record, deduplicated on (path, registry root) and sorted.
/// Test: `scan_reports_the_excluded_set_with_reasons`,
/// `scan_reports_both_registries_for_the_same_path_prefix`.
pub(crate) fn scan_registered_worktrees(repos_root: &Path) -> Vec<ScannedWorktree> {
    let canonical_root = std::fs::canonicalize(repos_root).unwrap_or_else(|_| repos_root.into());
    let mut found: BTreeSet<(PathBuf, PathBuf, ScannedKey)> = BTreeSet::new();
    let Ok(owner_entries) = std::fs::read_dir(repos_root) else {
        return Vec::new();
    };
    for owner_entry in owner_entries.flatten() {
        let owner_path = owner_entry.path();
        if !owner_path.is_dir() {
            continue;
        }
        let Ok(repo_entries) = std::fs::read_dir(&owner_path) else {
            continue;
        };
        for repo_entry in repo_entries.flatten() {
            let repo_path = repo_entry.path();
            // The containment boundary below is expressed against the project's
            // RESOLVED path. A project directory that will not canonicalize
            // cannot bound anything, so the whole project is skipped rather
            // than falling back to the far wider `repos_root` — a failed
            // observation may only ever shrink what this function proposes.
            let Ok(canonical_project) = std::fs::canonicalize(&repo_path) else {
                continue;
            };
            for anchor in [repo_path.clone(), repo_path.join(BASE_CLONE_DIRNAME)] {
                scan_from_anchor(&anchor, &canonical_root, &canonical_project, &mut found);
            }
        }
    }
    found
        .into_iter()
        .map(|(path, registry_root, key)| ScannedWorktree {
            path,
            project: key.project,
            registry_root,
            branch: key.branch,
            admission: key.admission,
        })
        .collect()
}

/// The non-key remainder of a [`ScannedWorktree`], carried through the dedup
/// `BTreeSet` (#4288).
///
/// Why: deduplication is on (path, registry root) — the identity of the entry —
/// but `BTreeSet` needs the WHOLE tuple to be `Ord`, and the payload has to
/// ride along. Keeping it in a named struct rather than a bare tuple keeps
/// `scan_registered_worktrees`' final `map` readable.
/// What: the fields of [`ScannedWorktree`] that are not part of its identity.
/// Test: covered by every `scan_*` test.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ScannedKey {
    project: PathBuf,
    branch: Option<String>,
    admission: Admission,
}

/// Record every worktree registered at `anchor`, with its admission verdict.
///
/// Why: extracted so [`scan_registered_worktrees`]' two anchors share one rule
/// rather than duplicating the root-confirmation, filtering, and containment
/// logic — the duplication that let the old walk's five shapes drift apart
/// from each other.
/// What: no-ops unless `anchor` is a directory that is ITSELF the root of the
/// repository owning its registry (see [`registry_root_for`]); then records
/// every worktree, marking [`Admission::Admitted`] only for the non-bare,
/// non-main, non-prunable, non-`locked` ones whose canonicalized path is a
/// STRICT DESCENDANT of `canonical_project` (the managed project directory —
/// see [`enumerate_registered_worktrees`]' "positive assertion" section) and
/// also lies under `canonical_root`.
///
/// Both containment checks are applied even though the first normally implies
/// the second: `canonical_project` is derived by canonicalizing a `read_dir`
/// entry, so a symlinked project directory could resolve OUTSIDE the managed
/// root, and the pair keeps the sweep bounded in that case too.
/// Test: covered by every `enumerate_*` and `scan_*` test.
fn scan_from_anchor(
    anchor: &Path,
    canonical_root: &Path,
    canonical_project: &Path,
    found: &mut BTreeSet<(PathBuf, PathBuf, ScannedKey)>,
) {
    if !anchor.is_dir() {
        return;
    }
    // Confirm the anchor is a repository ROOT, not merely a directory inside
    // one — otherwise git walks up and reports a foreign repository's registry.
    let Some(root) = registry_root_for(anchor) else {
        return;
    };
    let canonical_anchor = std::fs::canonicalize(anchor).unwrap_or_else(|_| anchor.into());
    let canonical_repo_root = std::fs::canonicalize(&root).unwrap_or(root);
    if canonical_anchor != canonical_repo_root {
        return;
    }
    let Some(worktrees) = list_registered_worktrees(anchor) else {
        return;
    };
    for wt in worktrees {
        let admission = admission_for(&wt, canonical_root, canonical_project);
        let path = std::fs::canonicalize(&wt.path).unwrap_or_else(|_| wt.path.clone());
        found.insert((
            path,
            canonical_repo_root.clone(),
            ScannedKey {
                project: canonical_project.to_path_buf(),
                branch: wt.branch.clone(),
                admission,
            },
        ));
    }
}

/// The [`Admission`] verdict for one porcelain record (#4288).
///
/// Why: the exclusion rules were `continue` statements inside the scan loop, so
/// nothing could name WHICH rule fired. Splitting them out makes each verdict
/// reportable without changing any of them — the order and the conditions are
/// exactly those `collect_from_anchor` applied before #4288.
/// What: applies, in order, the git-reported vetoes, then canonicalization,
/// then the two containment checks.
/// Test: `scan_reports_the_excluded_set_with_reasons`,
/// `enumerate_excludes_a_locked_worktree`.
fn admission_for(
    wt: &RegisteredWorktree,
    canonical_root: &Path,
    canonical_project: &Path,
) -> Admission {
    {
        // `locked` is git's own removal veto — the operator ran
        // `git worktree lock` to say "do not remove this".
        //
        // Be precise about WHY skipping here is load-bearing, because the
        // obvious reading is wrong and would get this filter deleted as
        // redundant. `git worktree remove --force` does NOT override a lock:
        // verified on git 2.54.0, a single `--force` exits 128 with "cannot
        // remove a locked working tree; use 'remove -f -f' to override or
        // unlock first", leaving the directory intact. Git respects the lock.
        //
        // The danger was what `decommission::remove_session_worktree` did with
        // that refusal: until #4732 it treated ANY non-zero git exit as "git
        // failed" and fell back to `std::fs::remove_dir_all`, deleting the
        // directory regardless — defeating the lock via the error path rather
        // than the force flag, AND leaving git's now-dangling registry entry
        // behind. The remover refuses now, so this filter is no longer the ONLY
        // place the veto survives; it is the first, and it is what keeps a
        // locked worktree out of every candidate list an operator is shown.
        if wt.bare {
            return Admission::Bare;
        }
        if wt.is_main {
            return Admission::MainCheckout;
        }
        if wt.prunable {
            return Admission::Prunable;
        }
        if wt.locked {
            // #6561: which KIND of lock, because only one of them names a
            // dispatched agent the operator must be told about.
            let harness = wt
                .lock_reason
                .as_deref()
                .is_some_and(is_harness_agent_lock_reason)
                && super::worktree_ownership::is_harness_agent_worktree(&wt.path);
            return if harness {
                Admission::HarnessAgentLock
            } else {
                Admission::Locked
            };
        }
        // #1845 item 8, preserved: a path that cannot be canonicalized is
        // SKIPPED, never proposed. A dangling symlink, a deletion race, an
        // unreadable parent, or a stalled mount all make the path unresolvable,
        // and an unresolvable path cannot be compared against the active-session
        // set — so proposing it would risk offering a LIVE worktree for
        // deletion. The asymmetry is deliberate: a failed observation reduces
        // what this function proposes, and can never enlarge it.
        let Ok(canonical) = std::fs::canonicalize(&wt.path) else {
            return Admission::Unresolvable;
        };
        // The structural boundary (#4224 HIGH). `starts_with` is component-wise,
        // so `<owner>/trusty-tools-worktrees/x` cannot match the project
        // `<owner>/trusty-tools`. The `!=` makes it STRICT: the project checkout
        // itself — the operator's working copy — is never a candidate, whether
        // or not some other registry lists it as a linked worktree.
        if canonical == canonical_project || !canonical.starts_with(canonical_project) {
            return Admission::OutsideProject;
        }
        if !canonical.starts_with(canonical_root) {
            return Admission::OutsideReposRoot;
        }
        Admission::Admitted
    }
}

#[cfg(test)]
#[path = "worktree_registry_tests.rs"]
mod worktree_registry_tests;
