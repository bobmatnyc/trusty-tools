//! Detect a session whose OWN worktree has been destroyed underneath it
//! (#3764 item 4).
//!
//! ## Why
//!
//! Every guard shipped before this one asks "should I delete that?". None of
//! them asks "is MY tree still there?". That gap is why the `f443c12d` session
//! ran for **three days** inside a worktree that had lost its `.git` pointer,
//! its registration, and its entire tracked source tree — while every harness
//! surface reported it healthy.
//!
//! The reason it read as healthy is a textbook fail-open. When a worktree's
//! `.git` pointer file is gone, git's repository discovery does not fail — it
//! walks UP to the enclosing `.base` clone and answers from there:
//!
//! * `git log` succeeds and prints plausible commits (from `.base`'s stale
//!   local `main`), so history looks fine.
//! * `git status` fatals with `this operation must be run in a work tree`,
//!   which the harness renders as `Status: (clean)` — a hard error laundered
//!   into a clean bill of health.
//!
//! ## What (and why NOT `--is-inside-work-tree` alone)
//!
//! The obvious probe is `git rev-parse --is-inside-work-tree`, which does
//! return `false` for a stripped worktree under this repo's layout. But that
//! answer is an artifact of `.base` being a **bare** clone, and it is NOT
//! layout-independent — verified empirically both ways:
//!
//! | parent repo | stripped worktree: `--is-inside-work-tree` | `--show-toplevel`  |
//! |-------------|--------------------------------------------|--------------------|
//! | bare        | `false`  → detected                        | fatal → detected   |
//! | **normal**  | **`true`** → **MISSED**                    | parent dir → detected |
//!
//! With a non-bare parent checkout, discovery lands on a directory that IS a
//! work tree, so `--is-inside-work-tree` cheerfully reports `true` for a
//! completely destroyed worktree. Shipping that probe alone would have
//! reproduced the very class of bug this issue exists to end: a detector that
//! reports healthy when it is not.
//!
//! This module therefore uses the layout-independent discriminator:
//! **`git -C <root> rev-parse --show-toplevel` must succeed AND resolve to
//! `<root>` itself.** Anything else — a fatal (bare parent), or a toplevel that
//! escaped to an ancestor (normal parent) — means this directory is no longer
//! its own worktree.
//!
//! ## Scope and fail-open avoidance
//!
//! Only SM-created worktrees ([`is_session_worktree`]) are checked, so a
//! local-path or adopted session pointed at a plain non-git directory can never
//! be misreported. A probe that could not run at all (no `git` binary) yields
//! [`WorktreeIntegrity::Unknown`], never `Intact` — an unobservable result is
//! never a passing result.
//!
//! Test: `classify_*` below (the pure matrix, including the non-bare-parent
//! case `--is-inside-work-tree` would miss) and
//! `worktree_integrity_tests.rs` (real git worktrees, both parent layouts).

use std::path::{Path, PathBuf};

use super::decommission::{
    GIT_WORKTREE_REMOVE_TIMEOUT, WORKTREE_SENTINEL_FILE, is_session_worktree,
};
use super::manager::SessionManager;
use super::record::{ManagedSessionId, ManagedSessionState};

/// Raw outcome of `git -C <root> rev-parse --show-toplevel`.
///
/// Why: separating "what git said" from "what that means" keeps the verdict
/// logic pure and unit-testable without spawning a process, and forces the
/// third case — git could not be consulted — to be handled explicitly instead
/// of collapsing into a false `Intact`.
/// What: [`Resolved`](Self::Resolved) — git printed a work-tree root.
/// [`NotAWorkTree`](Self::NotAWorkTree) — git RAN and gave one of the two
/// answers that definitively mean "this is not its own worktree any more"
/// (`not a work tree` / `not a git repository`).
/// [`Unavailable`](Self::Unavailable) — git could not be spawned, timed out, or
/// failed for some OTHER reason (dubious ownership, unreadable gitdir), none of
/// which prove anything either way.
/// Test: `classify_*` below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TopLevel {
    /// git resolved a work-tree root (already canonicalized by the caller).
    Resolved(PathBuf),
    /// git ran and definitively reported there is no work tree here.
    NotAWorkTree,
    /// git could not be consulted — no evidence in either direction.
    Unavailable(String),
}

/// Verdict on whether a session's worktree is still a real worktree.
///
/// Why: two distinctions are load-bearing here, and collapsing either one turns
/// a detector into a destroyer.
///
/// * `Unknown` vs everything else: an unobservable probe must never be reported
///   as healthy. `bool` would have forced that lie.
/// * [`LinkageLost`](Self::LinkageLost) vs [`Destroyed`](Self::Destroyed)
///   (code-critic HIGH-3 on the #3764 PR): "git metadata is gone" and "the files
///   are gone" are different states with OPPOSITE remediations. The first draft
///   had one variant whose alarm asserted *"Its uncommitted work is GONE. Stop
///   the session and recreate it"* — advice that, followed on a tree whose files
///   were entirely intact, would itself destroy the work. That is not
///   hypothetical for this repo: when the `.base` bare clone was destroyed on
///   07-21 it orphaned ~70 worktrees whose contents were **fully intact**, and
///   every one of them would have alarmed that way. Deleting only the base's
///   admin gitdir reproduces it exactly. Work is only ever claimed lost when the
///   directory has been observed empty.
///
/// What: [`Intact`](Self::Intact), [`LinkageLost`](Self::LinkageLost) (git
/// linkage gone, files still present — recover, do NOT decommission),
/// [`Destroyed`](Self::Destroyed) (linkage gone AND the directory is empty),
/// [`Unknown`](Self::Unknown) (probe inconclusive).
/// Test: `classify_*` below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorktreeIntegrity {
    /// The path is its own git work tree — healthy.
    Intact,
    /// Git linkage is gone but files SURVIVE on disk. Recoverable; must not be
    /// decommissioned. `surviving` is the non-sentinel entry count, or `None`
    /// when the directory could not be read (still never "work is gone").
    LinkageLost {
        detail: String,
        surviving: Option<usize>,
    },
    /// Git linkage is gone AND the directory is empty or absent — work is lost.
    Destroyed(String),
    /// The probe could not be completed. NEVER treat as healthy.
    Unknown(String),
}

impl WorktreeIntegrity {
    /// True when this verdict warrants an ERROR-level alarm.
    ///
    /// Why: `LinkageLost` and `Destroyed` are both operator-actionable failures
    /// that must reach `errors.jsonl`; `Intact` and `Unknown` are not (the
    /// latter is warned per-finding and rolled up separately — see
    /// [`alarm_rollup`]).
    /// What: `matches!(self, LinkageLost { .. } | Destroyed(_))`.
    /// Test: `rollup_reports_destroyed_sessions`,
    /// `rollup_reports_linkage_lost`, `rollup_is_silent_for_single_unknown`.
    pub(crate) fn is_alarm(&self) -> bool {
        matches!(self, Self::LinkageLost { .. } | Self::Destroyed(_))
    }
}

/// Decide integrity from a canonicalized root, a [`TopLevel`] probe result, and
/// the root's surviving-entry count.
///
/// Why: this is the whole detector, expressed with zero I/O so every branch —
/// including the non-bare-parent case that defeats `--is-inside-work-tree`, and
/// the files-intact case that must never be reported as lost work — can be
/// pinned by a unit test.
/// What: `root_canon == None` (the root does not resolve on disk at all) →
/// `Destroyed`, since a directory that is not there holds nothing. Otherwise:
/// `Unavailable` → `Unknown`; `Resolved(t)` with `t == root` → `Intact`;
/// `NotAWorkTree` or an escaped `Resolved(t)` → linkage is gone, and the
/// `surviving` count decides which of the two failure verdicts applies —
/// `Some(0)` → `Destroyed`, `Some(n>0)` or `None` (unreadable) →
/// `LinkageLost`. The unreadable case deliberately lands on `LinkageLost`: not
/// having counted the files is never grounds for telling an operator they are
/// gone.
/// Test: `classify_missing_root_is_destroyed`,
/// `classify_empty_dir_is_destroyed`,
/// `classify_surviving_files_is_linkage_lost`,
/// `classify_unreadable_dir_is_linkage_lost_not_destroyed`,
/// `classify_escaped_toplevel_with_files_is_linkage_lost`,
/// `classify_matching_toplevel_is_intact`,
/// `classify_unavailable_git_is_unknown_not_intact`.
pub(crate) fn classify(
    root_canon: Option<&Path>,
    probe: &TopLevel,
    surviving: Option<usize>,
) -> WorktreeIntegrity {
    let Some(root) = root_canon else {
        return WorktreeIntegrity::Destroyed(
            "workspace root does not exist on disk (cannot canonicalize)".into(),
        );
    };
    let detail = match probe {
        TopLevel::Unavailable(why) => {
            return WorktreeIntegrity::Unknown(format!(
                "could not run git to verify the worktree: {why}"
            ));
        }
        TopLevel::Resolved(top) if top == root => return WorktreeIntegrity::Intact,
        TopLevel::NotAWorkTree => {
            "git reports this path is not inside a work tree — its .git linkage is gone \
             (git discovery falls through to the enclosing repo, which is why `git log` \
             still appears to work here)"
                .to_string()
        }
        TopLevel::Resolved(top) => format!(
            "git discovery escaped this directory and resolved to {} instead — this path \
             is no longer its own worktree (note: `git rev-parse --is-inside-work-tree` \
             reports `true` here and would MISS this)",
            top.display()
        ),
    };
    match surviving {
        Some(0) => WorktreeIntegrity::Destroyed(format!("{detail}; the directory is EMPTY")),
        other => WorktreeIntegrity::LinkageLost {
            detail,
            surviving: other,
        },
    }
}

/// Count entries under `root`, ignoring the ownership sentinel.
///
/// Why: the sentinel is written by the session manager itself, so a directory
/// holding nothing but a sentinel holds no USER work — counting it would make
/// an empty, genuinely-destroyed worktree look recoverable and suppress the
/// correct alarm. `None` (unreadable) is propagated rather than defaulted to 0,
/// because defaulting would claim work is gone on no evidence.
/// What: `read_dir(root)` counting entries whose file name is not
/// [`WORKTREE_SENTINEL_FILE`]; `None` on any read error.
/// Test: `classify_empty_dir_is_destroyed`,
/// `linkage_lost_with_surviving_files_does_not_claim_work_gone` in
/// `worktree_integrity_tests.rs`.
pub(crate) fn count_surviving_entries(root: &Path) -> Option<usize> {
    let entries = std::fs::read_dir(root).ok()?;
    Some(
        entries
            .filter_map(Result::ok)
            .filter(|e| e.file_name() != WORKTREE_SENTINEL_FILE)
            .count(),
    )
}

/// The exact git stderr phrases that DEFINITIVELY mean "this path is not its
/// own work tree" (#3764, code-critic HIGH-3).
///
/// Why: `probe_toplevel` originally mapped ANY non-zero exit to
/// "not a work tree", which silently turned unrelated failures (dubious
/// ownership, an unreadable gitdir, a git bug) into a destruction verdict —
/// the root of the over-claiming alarm. Matching named phrases instead means an
/// unrecognised failure falls through to `Unavailable`/`Unknown`, which is the
/// safe direction: a detector that guesses "destroyed" tells operators to
/// delete things.
/// What: the three observed phrasings, matched case-insensitively as
/// substrings. `must be run in a work tree` is what a BARE parent produces (this
/// repo's `.base` layout); `not a git repository` is what a deleted admin gitdir
/// produces; `not a work tree` covers the older wording.
/// Test: `stripped_worktree_under_bare_parent_is_destroyed` and
/// `linkage_lost_with_surviving_files_does_not_claim_work_gone` in
/// `worktree_integrity_tests.rs` exercise the first two against real git.
const NOT_A_WORKTREE_STDERR: &[&str] = &[
    "must be run in a work tree",
    "not a work tree",
    "not a git repository",
];

/// Run `git -C <root> rev-parse --show-toplevel` and classify the answer.
///
/// Why: the single I/O site, kept trivial so [`classify`] holds all the logic.
/// What: success canonicalizes the printed path so the comparison in
/// [`classify`] is symlink-safe (macOS `/tmp` ↔ `/private/tmp`). A non-zero exit
/// is NOT blanket-treated as "no work tree" (code-critic HIGH-3): only stderr
/// naming `not a work tree` / `not a git repository` is definitive; every other
/// failure (dubious ownership, unreadable gitdir, a git bug) is
/// [`TopLevel::Unavailable`], because misreading those as destruction is how a
/// detector starts telling operators to delete intact work. A spawn failure
/// means git never ran → `Unavailable`.
/// Test: exercised end-to-end by `worktree_integrity_tests.rs` against real
/// git worktrees, including the admin-gitdir-deleted case.
pub(crate) fn probe_toplevel(root: &Path) -> TopLevel {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--show-toplevel"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let raw = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let path = PathBuf::from(&raw);
            TopLevel::Resolved(path.canonicalize().unwrap_or(path))
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr).to_lowercase();
            if NOT_A_WORKTREE_STDERR
                .iter()
                .any(|phrase| stderr.contains(phrase))
            {
                TopLevel::NotAWorkTree
            } else {
                TopLevel::Unavailable(format!(
                    "git rev-parse failed ({}) without a definitive answer: {}",
                    o.status,
                    stderr.trim()
                ))
            }
        }
        Err(e) => TopLevel::Unavailable(e.to_string()),
    }
}

/// Full integrity check for one worktree root.
///
/// Why: the composition callers want — canonicalize, probe, count, classify.
/// What: canonicalizes `root`, probes git against the ORIGINAL path
/// (canonicalization may fail while the path is still traversable), counts
/// surviving entries, and classifies.
/// Test: `worktree_integrity_tests.rs`.
pub(crate) fn check(root: &Path) -> WorktreeIntegrity {
    let canon = root.canonicalize().ok();
    classify(
        canon.as_deref(),
        &probe_toplevel(root),
        count_surviving_entries(root),
    )
}

/// One session whose worktree failed the integrity check.
///
/// Why: the sweep returns what it found so the caller (and tests) can assert
/// on structured results rather than scraping logs.
/// What: the session id, its workspace path, and the verdict.
/// Test: `audit_flags_destroyed_worktree` in `worktree_integrity_tests.rs`.
#[derive(Debug, Clone)]
pub(crate) struct IntegrityFinding {
    pub id: ManagedSessionId,
    pub path: PathBuf,
    pub verdict: WorktreeIntegrity,
}

/// Consecutive audit ticks a session stays in a bad state before the alarm
/// repeats (#3764, code-critic HIGH-4).
///
/// Why: the first draft re-alarmed every tick with no latch. At the ~60 s
/// orphan-GC cadence one persistent finding emits ~2 880 ERROR events/day into
/// the 500-entry, NON-deduping capture ring (`error_capture/store.rs`), which
/// fully rotates in about four hours — evicting every other captured error from
/// `list_recent_errors`, `preview_bug_report`, and `tm doctor`. That degrades
/// the exact surface this PR's whole ERROR-level argument depends on: the alarm
/// would drown out everything, including itself. The last incident persisted
/// three DAYS, so this is the expected case, not a corner.
/// What: alarm on TRANSITION into the bad state (streak 1), then once every
/// [`REALARM_EVERY_TICKS`] ticks — ~1 h at the default cadence, ~24 events/day
/// instead of ~2 880, while still proving the condition is ongoing. Mirrors the
/// per-path streak map `prune.rs` already uses for the #1845 F3 alarm.
/// Test: `latch_alarms_on_transition_then_suppresses`,
/// `latch_realarms_after_interval`, `latch_resets_when_session_recovers`.
const REALARM_EVERY_TICKS: u32 = 60;

/// Per-session consecutive-bad-tick counter backing the alarm latch (#3764).
///
/// Why: see [`REALARM_EVERY_TICKS`]. Deliberately in-process and unpersisted,
/// exactly like `prune.rs`'s streak map — a daemon restart re-establishes a
/// clean baseline and the first post-restart tick re-alarms, which is the
/// correct behaviour for a condition that is still present.
/// What: `record_bad` increments and returns whether THIS tick should alarm;
/// `retain` drops sessions no longer in the audited set (recovered, stopped, or
/// decommissioned) so a session that breaks again later alarms immediately
/// rather than inheriting a suppressed streak.
/// Test: `latch_alarms_on_transition_then_suppresses`,
/// `latch_realarms_after_interval`, `latch_resets_when_session_recovers`.
#[derive(Debug, Default)]
pub(crate) struct AlarmLatch {
    ticks: std::collections::HashMap<ManagedSessionId, u32>,
}

impl AlarmLatch {
    /// Record one more consecutive bad tick for `id`; `true` = alarm now.
    pub(crate) fn record_bad(&mut self, id: ManagedSessionId) -> bool {
        let count = self.ticks.entry(id).or_insert(0);
        *count += 1;
        *count == 1 || (*count).is_multiple_of(REALARM_EVERY_TICKS)
    }

    /// Drop every tracked session not in `still_bad`.
    pub(crate) fn retain(&mut self, still_bad: &std::collections::HashSet<ManagedSessionId>) {
        self.ticks.retain(|id, _| still_bad.contains(id));
    }
}

/// Process-global [`AlarmLatch`] shared by every audit call site.
static ALARM_LATCH: std::sync::OnceLock<std::sync::Mutex<AlarmLatch>> = std::sync::OnceLock::new();

fn alarm_latch() -> &'static std::sync::Mutex<AlarmLatch> {
    ALARM_LATCH.get_or_init(|| std::sync::Mutex::new(AlarmLatch::default()))
}

impl SessionManager {
    /// Check every Active session's own worktree and alarm on any whose git
    /// linkage or contents have been destroyed (#3764 item 4).
    ///
    /// Why: this is the detector that would have caught all three worktree-loss
    /// incidents within one GC interval instead of three days. It runs on the
    /// daemon's existing orphan-GC tick, which already walks the same records —
    /// no new loop, no new configuration, and the first tick after daemon start
    /// doubles as the at-start self-check.
    ///
    /// The `error!` level is load-bearing, not stylistic: the daemon composes
    /// `trusty_common::error_capture::bug_capture_layer`, which persists ONLY
    /// ERROR-level events to `<data_dir>/trusty-mpm/errors.jsonl` and surfaces
    /// them via the `list_recent_errors` MCP tool and `tm doctor`. At WARN this
    /// finding would be invisible to every operator surface — the same silence
    /// that let the last incident run for three days.
    ///
    /// What: delegates to [`Self::audit_worktree_integrity_with_timeout`] with
    /// the shared [`GIT_WORKTREE_REMOVE_TIMEOUT`] bound.
    /// Test: `audit_flags_destroyed_worktree`, `audit_passes_healthy_worktree`,
    /// `audit_ignores_non_worktree_workspace`,
    /// `audit_times_out_into_unknown_never_destroyed` in
    /// `worktree_integrity_tests.rs`.
    pub(crate) async fn audit_worktree_integrity(&self) -> Vec<IntegrityFinding> {
        self.audit_worktree_integrity_with_timeout(GIT_WORKTREE_REMOVE_TIMEOUT)
            .await
    }

    /// [`Self::audit_worktree_integrity`] with an injectable probe timeout.
    ///
    /// Why (code-critic HIGH-2): the probe shells out to `git`, and the first
    /// draft awaited it unbounded from inside `orphan_gc_loop`. One worktree on
    /// a stalled network mount would block the audit, everything after it in
    /// that tick (the search-index sweep), and every future tick — indefinitely.
    /// The timeout is the SAME [`GIT_WORKTREE_REMOVE_TIMEOUT`] constant
    /// `remove_session_worktree` already uses for the same hazard under #1845
    /// item 4, rather than a second independent knob. Injectable purely so the
    /// elapsed branch is testable without a real hang.
    /// What: per candidate, `tokio::time::timeout(probe_timeout, spawn_blocking(check))`.
    /// An elapsed probe yields [`WorktreeIntegrity::Unknown`] — never a
    /// destruction verdict, since a timeout is an absence of evidence, and never
    /// `Intact`. Findings are latched (see [`AlarmLatch`]) so a persistent
    /// condition cannot flood the error ring. Never mutates anything: a
    /// destroyed worktree is reported, never "repaired", because silently
    /// recreating it is exactly the #3715 masking behaviour.
    /// Test: `audit_times_out_into_unknown_never_destroyed`.
    pub(crate) async fn audit_worktree_integrity_with_timeout(
        &self,
        probe_timeout: std::time::Duration,
    ) -> Vec<IntegrityFinding> {
        let candidates: Vec<(ManagedSessionId, PathBuf)> = self
            .list()
            .await
            .into_iter()
            .filter(|r| matches!(r.state, ManagedSessionState::Active))
            .filter_map(|r| r.workspace_path.map(|p| (r.id, p)))
            .filter(|(_, p)| is_session_worktree(p))
            .collect();
        let candidate_count = candidates.len();

        let mut findings = Vec::new();
        for (id, path) in candidates {
            let probe_path = path.clone();
            let join = tokio::task::spawn_blocking(move || check(&probe_path));
            let verdict = match tokio::time::timeout(probe_timeout, join).await {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    WorktreeIntegrity::Unknown(format!("integrity probe task failed: {e}"))
                }
                Err(_elapsed) => WorktreeIntegrity::Unknown(format!(
                    "integrity probe timed out after {}s — git may be blocked on a stalled \
                     mount; NOT a clean bill of health",
                    probe_timeout.as_secs()
                )),
            };
            if matches!(verdict, WorktreeIntegrity::Intact) {
                continue;
            }
            findings.push(IntegrityFinding { id, path, verdict });
        }

        // Latch BEFORE emitting, so one persistent finding cannot rotate the
        // 500-entry capture ring (HIGH-4). Only sessions that alarm this tick
        // are rendered; the rest are counted and stay silent.
        let alarming: Vec<&IntegrityFinding> = {
            let mut latch = alarm_latch()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let still_bad: std::collections::HashSet<ManagedSessionId> =
                findings.iter().map(|f| f.id).collect();
            latch.retain(&still_bad);
            findings.iter().filter(|f| latch.record_bad(f.id)).collect()
        };
        for finding in &alarming {
            emit_finding_alarm(finding);
        }
        alarm_rollup(&alarming, &findings, candidate_count);
        findings
    }
}

/// Render one latched finding at its correct level and with remediation that
/// matches the verdict (#3764).
///
/// Why: the `LinkageLost` / `Destroyed` split exists precisely so the two get
/// DIFFERENT advice (code-critic HIGH-3). Keeping both messages in one function
/// makes it obvious that only the empty-directory case is ever told its work is
/// gone.
/// What: `Destroyed` → ERROR asserting the work is lost and recreation is safe;
/// `LinkageLost` → ERROR that explicitly says files SURVIVE and must be
/// recovered BEFORE any decommission; `Unknown` → WARN (rolled up separately);
/// `Intact` → unreachable (filtered by the caller).
/// Test: `linkage_lost_with_surviving_files_does_not_claim_work_gone` in
/// `worktree_integrity_tests.rs`.
fn emit_finding_alarm(finding: &IntegrityFinding) {
    let (id, path) = (finding.id, finding.path.as_path());
    match &finding.verdict {
        WorktreeIntegrity::Intact => {}
        WorktreeIntegrity::Destroyed(detail) => {
            tracing::error!(
                session = %id,
                path = %path.display(),
                detail = %detail,
                "WORKTREE DESTROYED: an ACTIVE session's own worktree is no longer a git \
                 work tree AND the directory is empty. This session is running blind — \
                 `git log` still answers from the enclosing repo and `git status` fatals, \
                 which renders as a clean status. Its uncommitted work is gone. Stop the \
                 session and recreate it; do not let it keep committing (#3764/#3715)"
            );
        }
        WorktreeIntegrity::LinkageLost { detail, surviving } => {
            tracing::error!(
                session = %id,
                path = %path.display(),
                surviving = surviving.map_or(-1_i64, |n| n as i64),
                detail = %detail,
                "WORKTREE LINKAGE LOST: an ACTIVE session's worktree is no longer a git \
                 work tree, but its FILES ARE STILL ON DISK ({}). The work is NOT lost — \
                 DO NOT decommission or recreate this session, which would delete it. \
                 Copy the directory aside first, then re-link or re-create the worktree. \
                 A destroyed parent clone produces exactly this state across many \
                 worktrees at once (#3764/#3715)",
                surviving.map_or_else(
                    || "entry count unavailable".to_string(),
                    |n| format!("{n} entries")
                )
            );
        }
        WorktreeIntegrity::Unknown(why) => {
            tracing::warn!(
                session = %id,
                path = %path.display(),
                "worktree integrity could not be verified (NOT a clean bill of health): \
                 {why} (#3764)"
            );
        }
    }
}

/// Emit the sweep-level ERROR roll-up (#3764).
///
/// Why: the per-finding alarms carry the evidence, but an operator scanning a
/// log (or `list_recent_errors`) wants ONE line for the sweep. Kept beside the
/// audit — rather than inline in `daemon::orphan_gc_loop` — so the "which
/// findings are loud enough to roll up" rule lives next to the verdicts it
/// filters, and the daemon loop stays a call site rather than a second place
/// this policy is expressed.
///
/// What: two independent ERROR conditions.
///
/// 1. **Damage.** Rolls up the findings that alarmed THIS tick and are
///    [`WorktreeIntegrity::is_alarm`] — `Destroyed` or `LinkageLost`.
///    Suppressed findings (latched, see [`AlarmLatch`]) are excluded so the
///    roll-up cannot become the flood the latch exists to prevent.
/// 2. **The detector is down** (code-critic MEDIUM). When there were
///    candidates to audit and EVERY verdict came back `Unknown`, the detector
///    itself is dark — `git` unavailable daemon-wide, or every probe timing
///    out. Individually those are `warn!`, which this PR's own rationale says
///    reaches no operator surface; collectively they mean the guard is not
///    guarding, which is exactly the fail-open the module doc argues against.
///    That is a distinct condition from the alarm-fatigue case, so it is ERROR
///    once per sweep rather than per finding.
///
/// A lone `Unknown` among healthy sessions stays quiet — an alarm that fires on
/// "git was briefly unavailable for one worktree" gets muted, and a muted alarm
/// is a silent one.
/// Test: `rollup_reports_destroyed_sessions`, `rollup_reports_linkage_lost`,
/// `rollup_is_silent_for_single_unknown`,
/// `rollup_errors_when_every_verdict_is_unknown`.
fn alarm_rollup(alarming: &[&IntegrityFinding], all: &[IntegrityFinding], candidates: usize) {
    let damaged: Vec<String> = alarming
        .iter()
        .filter(|f| f.verdict.is_alarm())
        // `<session-id>=<path>` so the roll-up alone tells an operator WHICH
        // sessions are affected and where they were running.
        .map(|f| format!("{}={}", f.id, f.path.display()))
        .collect();
    if !damaged.is_empty() {
        tracing::error!(
            damaged = damaged.len(),
            sessions = %damaged.join(" "),
            "worktree-integrity audit: {} ACTIVE session(s) have a destroyed or \
             unlinked worktree (#3764)",
            damaged.len()
        );
    }

    // The detector-is-down case: every audited session came back Unknown, so
    // nothing at all was actually verified this sweep.
    let all_unknown = candidates > 0
        && all.len() == candidates
        && all
            .iter()
            .all(|f| matches!(f.verdict, WorktreeIntegrity::Unknown(_)));
    if all_unknown {
        tracing::error!(
            candidates,
            "worktree-integrity audit is DARK: all {candidates} audited session(s) \
             returned an inconclusive verdict, so no worktree is currently being \
             verified. Check that `git` is on the daemon's PATH and that no workspace \
             is on a stalled mount (#3764)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WT: &str = "/managed/base/.worktrees/wt";

    fn root() -> PathBuf {
        PathBuf::from(WT)
    }

    // ── classify: linkage-lost vs destroyed (code-critic HIGH-3) ────────────

    /// A root that does not resolve on disk at all is Destroyed.
    ///
    /// Why: a directory that is not there holds nothing, so claiming the work
    /// is gone is accurate in this one case.
    #[test]
    fn classify_missing_root_is_destroyed() {
        assert!(matches!(
            classify(None, &TopLevel::NotAWorkTree, Some(0)),
            WorktreeIntegrity::Destroyed(_)
        ));
    }

    /// Linkage gone AND the directory empty → Destroyed.
    #[test]
    fn classify_empty_dir_is_destroyed() {
        assert!(matches!(
            classify(Some(&root()), &TopLevel::NotAWorkTree, Some(0)),
            WorktreeIntegrity::Destroyed(_)
        ));
    }

    /// Linkage gone but files SURVIVE → LinkageLost, never Destroyed.
    ///
    /// Why (code-critic HIGH-3): the two states have opposite remediations.
    /// Reporting this as `Destroyed` makes the alarm tell an operator their
    /// work is gone and to recreate the session — advice that would itself
    /// delete files still sitting on disk. The 07-21 `.base` destruction
    /// orphaned ~70 worktrees in exactly this state, contents fully intact.
    /// Test: this function IS the test.
    #[test]
    fn classify_surviving_files_is_linkage_lost() {
        match classify(Some(&root()), &TopLevel::NotAWorkTree, Some(7)) {
            WorktreeIntegrity::LinkageLost { surviving, .. } => {
                assert_eq!(surviving, Some(7), "the surviving count must be carried");
            }
            other => panic!("files on disk must be LinkageLost, not {other:?}"),
        }
    }

    /// An UNREADABLE directory is LinkageLost, never Destroyed.
    ///
    /// Why: not having counted the files is never grounds for telling an
    /// operator they are gone. The unreadable case must fail toward "recover",
    /// not toward "recreate".
    /// Test: this function IS the test.
    #[test]
    fn classify_unreadable_dir_is_linkage_lost_not_destroyed() {
        let verdict = classify(Some(&root()), &TopLevel::NotAWorkTree, None);
        match verdict {
            WorktreeIntegrity::LinkageLost { surviving, .. } => assert_eq!(surviving, None),
            other => panic!("an uncounted directory must not be Destroyed; got {other:?}"),
        }
    }

    /// The NON-BARE-parent shape (discovery escapes upward) with files present
    /// is LinkageLost and names where discovery went.
    ///
    /// Why: this is the case `git rev-parse --is-inside-work-tree` returns
    /// `true` for (verified empirically). Detecting it is the entire reason
    /// this module compares `--show-toplevel` against the root instead of
    /// trusting the simpler probe.
    /// Test: this function IS the test.
    #[test]
    fn classify_escaped_toplevel_with_files_is_linkage_lost() {
        let escaped = TopLevel::Resolved(PathBuf::from("/managed/base"));
        match classify(Some(&root()), &escaped, Some(3)) {
            WorktreeIntegrity::LinkageLost { detail, surviving } => {
                assert_eq!(surviving, Some(3));
                assert!(
                    detail.contains("/managed/base"),
                    "the verdict must name where discovery escaped to; got {detail:?}"
                );
            }
            other => panic!("an escaped toplevel with files must be LinkageLost, got {other:?}"),
        }
    }

    /// The healthy case: toplevel == root → Intact.
    #[test]
    fn classify_matching_toplevel_is_intact() {
        assert_eq!(
            classify(Some(&root()), &TopLevel::Resolved(root()), Some(9)),
            WorktreeIntegrity::Intact
        );
    }

    /// A probe that could not run is Unknown — NEVER Intact, never Destroyed.
    ///
    /// Why: "the check didn't run" reported as "the check passed" is the exact
    /// fail-open shape this whole issue is about. An unobservable result is
    /// never a passing result — and never a destruction verdict either.
    /// Test: this function IS the test.
    #[test]
    fn classify_unavailable_git_is_unknown_not_intact() {
        let verdict = classify(
            Some(&root()),
            &TopLevel::Unavailable("no such file".into()),
            Some(0),
        );
        assert!(
            matches!(verdict, WorktreeIntegrity::Unknown(_)),
            "an unrunnable probe must be Unknown; got {verdict:?}"
        );
        assert_ne!(verdict, WorktreeIntegrity::Intact);
    }

    // ── alarm latch (code-critic HIGH-4) ────────────────────────────────────

    /// The latch alarms on transition, then goes quiet.
    ///
    /// Why (HIGH-4): without this, one persistent finding emits ~2 880 ERROR
    /// events/day into a 500-entry non-deduping ring, rotating it in ~4 h and
    /// evicting every other error from `list_recent_errors` / `tm doctor` —
    /// degrading the very surface the ERROR-level argument depends on.
    /// Test: this function IS the test.
    #[test]
    fn latch_alarms_on_transition_then_suppresses() {
        let mut latch = AlarmLatch::default();
        let id = ManagedSessionId::new();
        assert!(latch.record_bad(id), "the first bad tick must alarm");
        for tick in 2..REALARM_EVERY_TICKS {
            assert!(
                !latch.record_bad(id),
                "tick {tick} must be suppressed by the latch"
            );
        }
    }

    /// The latch re-alarms once per interval, so an ongoing condition is still
    /// proven ongoing rather than going permanently silent.
    #[test]
    fn latch_realarms_after_interval() {
        let mut latch = AlarmLatch::default();
        let id = ManagedSessionId::new();
        let alarms = (0..REALARM_EVERY_TICKS * 2)
            .filter(|_| latch.record_bad(id))
            .count();
        assert_eq!(
            alarms,
            3,
            "expected transition + 2 interval re-alarms over {} ticks",
            REALARM_EVERY_TICKS * 2
        );
    }

    /// A session that recovers is evicted, so a later break alarms immediately
    /// instead of inheriting a suppressed streak.
    #[test]
    fn latch_resets_when_session_recovers() {
        let mut latch = AlarmLatch::default();
        let id = ManagedSessionId::new();
        assert!(latch.record_bad(id));
        assert!(!latch.record_bad(id));
        latch.retain(&std::collections::HashSet::new()); // recovered
        assert!(
            latch.record_bad(id),
            "a session that recovered and broke again must alarm immediately"
        );
    }

    // ── roll-up policy ──────────────────────────────────────────────────────

    /// Capture the levels of every tracing event emitted by `body`.
    fn levels_emitted_by(body: impl FnOnce()) -> Vec<tracing::Level> {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::layer::SubscriberExt as _;

        struct Collector(Arc<Mutex<Vec<tracing::Level>>>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Collector {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                self.0.lock().expect("lock").push(*event.metadata().level());
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(Collector(Arc::clone(&seen)));
        tracing::subscriber::with_default(subscriber, body);
        seen.lock().expect("lock").clone()
    }

    fn finding(verdict: WorktreeIntegrity) -> IntegrityFinding {
        IntegrityFinding {
            id: ManagedSessionId::new(),
            path: root(),
            verdict,
        }
    }

    /// A Destroyed finding produces an ERROR roll-up.
    #[test]
    fn rollup_reports_destroyed_sessions() {
        let f = finding(WorktreeIntegrity::Destroyed("gone".into()));
        let levels = levels_emitted_by(|| alarm_rollup(&[&f], std::slice::from_ref(&f), 1));
        assert!(
            levels.contains(&tracing::Level::ERROR),
            "a destroyed worktree must roll up at ERROR; got {levels:?}"
        );
    }

    /// A LinkageLost finding also rolls up at ERROR — it is damage too, just
    /// damage with a different remedy.
    #[test]
    fn rollup_reports_linkage_lost() {
        let f = finding(WorktreeIntegrity::LinkageLost {
            detail: "unlinked".into(),
            surviving: Some(4),
        });
        let levels = levels_emitted_by(|| alarm_rollup(&[&f], std::slice::from_ref(&f), 1));
        assert!(levels.contains(&tracing::Level::ERROR));
    }

    /// ONE Unknown among healthy sessions stays silent.
    ///
    /// Why: escalating "git was briefly unavailable for one worktree" trains
    /// operators to ignore the alarm, which converts a loud alarm into a silent
    /// one — the exact failure this issue exists to end.
    /// Test: this function IS the test.
    #[test]
    fn rollup_is_silent_for_single_unknown() {
        let f = finding(WorktreeIntegrity::Unknown("no git".into()));
        // 5 candidates audited, only this one inconclusive → detector is fine.
        let levels = levels_emitted_by(|| alarm_rollup(&[&f], std::slice::from_ref(&f), 5));
        assert!(
            levels.is_empty(),
            "a lone Unknown must not roll up at all; got {levels:?}"
        );
    }

    /// When EVERY audited session is Unknown, the detector itself is dark and
    /// that IS an ERROR (code-critic MEDIUM).
    ///
    /// Why: individually those are `warn!`, which this PR's own rationale says
    /// reaches no operator surface. Collectively they mean nothing is being
    /// verified — a silently dark detector is the fail-open the module doc
    /// argues against.
    /// Test: this function IS the test.
    #[test]
    fn rollup_errors_when_every_verdict_is_unknown() {
        let all = vec![
            finding(WorktreeIntegrity::Unknown("no git".into())),
            finding(WorktreeIntegrity::Unknown("no git".into())),
        ];
        let levels = levels_emitted_by(|| alarm_rollup(&[], &all, 2));
        assert!(
            levels.contains(&tracing::Level::ERROR),
            "an entirely-dark detector must ERROR once per sweep; got {levels:?}"
        );
    }
}
