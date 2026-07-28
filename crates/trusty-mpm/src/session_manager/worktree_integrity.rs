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

use super::decommission::is_session_worktree;
use super::manager::SessionManager;
use super::record::{ManagedSessionId, ManagedSessionState};

/// Raw outcome of `git -C <root> rev-parse --show-toplevel`.
///
/// Why: separating "what git said" from "what that means" keeps the verdict
/// logic pure and unit-testable without spawning a process, and forces the
/// third case — git could not be consulted — to be handled explicitly instead
/// of collapsing into a false `Intact`.
/// What: [`Resolved`](Self::Resolved) — git printed a work-tree root.
/// [`NotAWorkTree`](Self::NotAWorkTree) — git RAN and refused (exit != 0;
/// `fatal: this operation must be run in a work tree`, or `not a git
/// repository`). [`Unavailable`](Self::Unavailable) — git could not be spawned
/// at all, which proves nothing either way.
/// Test: `classify_*` below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TopLevel {
    /// git resolved a work-tree root (already canonicalized by the caller).
    Resolved(PathBuf),
    /// git ran and reported there is no work tree here.
    NotAWorkTree,
    /// git could not be run — no evidence in either direction.
    Unavailable(String),
}

/// Verdict on whether a session's worktree is still a real worktree.
///
/// Why: `Unknown` exists as a first-class variant precisely so an unobservable
/// probe can never be reported as healthy. `bool` would have forced that lie.
/// What: [`Intact`](Self::Intact), [`Destroyed`](Self::Destroyed) (with the
/// human-readable evidence), [`Unknown`](Self::Unknown) (with the reason).
/// Test: `classify_*` below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorktreeIntegrity {
    /// The path is its own git work tree — healthy.
    Intact,
    /// The path is NOT its own work tree any more. Carries the evidence.
    Destroyed(String),
    /// The probe could not be completed. NEVER treat as healthy.
    Unknown(String),
}

/// Decide integrity from a canonicalized root and a [`TopLevel`] probe result.
///
/// Why: this is the whole detector, expressed with zero I/O so every branch —
/// including the non-bare-parent case that defeats `--is-inside-work-tree` —
/// can be pinned by a unit test.
/// What: `root_canon == None` (the root does not resolve on disk at all) →
/// `Destroyed`. Otherwise: `Unavailable` → `Unknown`; `NotAWorkTree` →
/// `Destroyed`; `Resolved(t)` → `Intact` iff `t == root_canon`, else
/// `Destroyed` naming where discovery escaped to.
/// Test: `classify_missing_root_is_destroyed`,
/// `classify_not_a_work_tree_is_destroyed`,
/// `classify_escaped_toplevel_is_destroyed`,
/// `classify_matching_toplevel_is_intact`,
/// `classify_unavailable_git_is_unknown_not_intact`.
pub(crate) fn classify(root_canon: Option<&Path>, probe: &TopLevel) -> WorktreeIntegrity {
    let Some(root) = root_canon else {
        return WorktreeIntegrity::Destroyed(
            "workspace root does not exist on disk (cannot canonicalize)".into(),
        );
    };
    match probe {
        TopLevel::Unavailable(why) => {
            WorktreeIntegrity::Unknown(format!("could not run git to verify the worktree: {why}"))
        }
        TopLevel::NotAWorkTree => WorktreeIntegrity::Destroyed(
            "git reports this path is not inside a work tree — its .git pointer is gone \
             (git discovery fell through to the enclosing bare repo, which is why \
             `git log` still appears to work here)"
                .into(),
        ),
        TopLevel::Resolved(top) if top == root => WorktreeIntegrity::Intact,
        TopLevel::Resolved(top) => WorktreeIntegrity::Destroyed(format!(
            "git discovery escaped this directory and resolved to {} instead — this path \
             is no longer its own worktree (note: `git rev-parse --is-inside-work-tree` \
             reports `true` here and would MISS this)",
            top.display()
        )),
    }
}

/// Run `git -C <root> rev-parse --show-toplevel` and canonicalize the answer.
///
/// Why: the single I/O site, kept trivial so [`classify`] holds all the logic.
/// What: a non-zero exit means git ran and refused → [`TopLevel::NotAWorkTree`]
/// (definitive: git itself said there is no work tree here). A spawn failure
/// means git never ran → [`TopLevel::Unavailable`]. Success canonicalizes the
/// printed path so the comparison in [`classify`] is symlink-safe (macOS
/// `/tmp` ↔ `/private/tmp`).
/// Test: exercised end-to-end by `worktree_integrity_tests.rs` against real
/// git worktrees.
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
        // git RAN and refused — definitive evidence, not an inconclusive error.
        Ok(_) => TopLevel::NotAWorkTree,
        Err(e) => TopLevel::Unavailable(e.to_string()),
    }
}

/// Full integrity check for one worktree root.
///
/// Why: the composition callers want — canonicalize, probe, classify.
/// What: `classify(root.canonicalize().ok().as_deref(), &probe_toplevel(root))`.
/// Test: `worktree_integrity_tests.rs`.
pub(crate) fn check(root: &Path) -> WorktreeIntegrity {
    let canon = root.canonicalize().ok();
    // Probe the ORIGINAL path: canonicalization may fail while the path is
    // still traversable, and git resolves it the same way either way.
    classify(canon.as_deref(), &probe_toplevel(root))
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

impl SessionManager {
    /// Check every Active session's own worktree and alarm on any that has been
    /// destroyed (#3764 item 4).
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
    /// What: scans Active records with a `workspace_path` that
    /// [`is_session_worktree`] recognises (so local-path/adopted sessions on
    /// plain directories are never misjudged), runs [`check`] on each inside
    /// `spawn_blocking` (it shells out to git), `error!`s every `Destroyed`
    /// verdict and `warn!`s every `Unknown` one, and returns all non-`Intact`
    /// findings. It never mutates anything — a destroyed worktree is reported,
    /// never "repaired", because silently recreating it is exactly the #3715
    /// masking behaviour.
    /// Test: `audit_flags_destroyed_worktree`,
    /// `audit_passes_healthy_worktree`,
    /// `audit_ignores_non_worktree_workspace` in `worktree_integrity_tests.rs`.
    pub(crate) async fn audit_worktree_integrity(&self) -> Vec<IntegrityFinding> {
        let candidates: Vec<(ManagedSessionId, PathBuf)> = self
            .list()
            .await
            .into_iter()
            .filter(|r| matches!(r.state, ManagedSessionState::Active))
            .filter_map(|r| r.workspace_path.map(|p| (r.id, p)))
            .filter(|(_, p)| is_session_worktree(p))
            .collect();

        let mut findings = Vec::new();
        for (id, path) in candidates {
            let probe_path = path.clone();
            let verdict = match tokio::task::spawn_blocking(move || check(&probe_path)).await {
                Ok(v) => v,
                Err(e) => WorktreeIntegrity::Unknown(format!("integrity probe task failed: {e}")),
            };
            match &verdict {
                WorktreeIntegrity::Intact => continue,
                WorktreeIntegrity::Destroyed(detail) => {
                    tracing::error!(
                        session = %id,
                        path = %path.display(),
                        detail = %detail,
                        "WORKTREE DESTROYED: an ACTIVE session's own worktree is no longer a \
                         git work tree. This session is running blind — `git log` still \
                         answers from the enclosing repo and `git status` fatals, which \
                         renders as a clean status. Its uncommitted work is GONE. Stop the \
                         session and recreate it; do not let it keep committing (#3764/#3715)"
                    );
                }
                WorktreeIntegrity::Unknown(why) => {
                    tracing::warn!(
                        session = %id,
                        path = %path.display(),
                        "worktree integrity could not be verified (NOT a clean bill of \
                         health): {why} (#3764)"
                    );
                }
            }
            findings.push(IntegrityFinding { id, path, verdict });
        }
        alarm_rollup(&findings);
        findings
    }
}

/// Emit the sweep-level ERROR roll-up for the `Destroyed` findings (#3764).
///
/// Why: the per-finding alarms carry the evidence, but an operator scanning a
/// log (or `list_recent_errors`) wants ONE line that says how many sessions are
/// affected and which. Kept as a free function beside the audit — rather than
/// inline in `daemon::orphan_gc_loop` — so the "which findings are loud enough
/// to roll up" rule lives next to the verdicts it is filtering, and so the
/// daemon loop stays a call site rather than a second place this policy is
/// expressed.
/// What: rolls up ONLY [`WorktreeIntegrity::Destroyed`]. An [`Unknown`]
/// (`WorktreeIntegrity::Unknown`) is already warned per-finding and is
/// deliberately NOT escalated here: an alarm that fires on "git was briefly
/// unavailable" gets muted by operators, and a muted alarm is a silent one —
/// the failure mode this whole issue exists to end. Emits nothing when no
/// worktree is destroyed.
/// Test: `rollup_reports_destroyed_sessions`,
/// `rollup_is_silent_for_unknown_only` below.
fn alarm_rollup(findings: &[IntegrityFinding]) {
    let destroyed: Vec<String> = findings
        .iter()
        .filter(|f| matches!(f.verdict, WorktreeIntegrity::Destroyed(_)))
        // `<session-id>=<path>` so the roll-up alone tells an operator WHICH
        // sessions to stop and where they were running.
        .map(|f| format!("{}={}", f.id, f.path.display()))
        .collect();
    if destroyed.is_empty() {
        return;
    }
    tracing::error!(
        destroyed = destroyed.len(),
        sessions = %destroyed.join(" "),
        "worktree-integrity audit: {} ACTIVE session(s) are running inside a destroyed \
         worktree (#3764)",
        destroyed.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A root that does not resolve on disk is Destroyed, not Unknown.
    #[test]
    fn classify_missing_root_is_destroyed() {
        assert!(matches!(
            classify(None, &TopLevel::NotAWorkTree),
            WorktreeIntegrity::Destroyed(_)
        ));
    }

    /// The BARE-parent shape: git refuses → Destroyed.
    #[test]
    fn classify_not_a_work_tree_is_destroyed() {
        let root = PathBuf::from("/managed/base/.worktrees/wt");
        assert!(matches!(
            classify(Some(&root), &TopLevel::NotAWorkTree),
            WorktreeIntegrity::Destroyed(_)
        ));
    }

    /// The NON-BARE-parent shape: discovery escapes upward → Destroyed.
    ///
    /// Why: this is the case `git rev-parse --is-inside-work-tree` returns
    /// `true` for (verified empirically). Detecting it is the entire reason
    /// this module compares `--show-toplevel` against the root instead of
    /// trusting the simpler probe.
    /// Test: this function IS the test.
    #[test]
    fn classify_escaped_toplevel_is_destroyed() {
        let root = PathBuf::from("/managed/base/.worktrees/wt");
        let escaped = TopLevel::Resolved(PathBuf::from("/managed/base"));
        match classify(Some(&root), &escaped) {
            WorktreeIntegrity::Destroyed(detail) => {
                assert!(
                    detail.contains("/managed/base"),
                    "the verdict must name where discovery escaped to; got {detail:?}"
                );
            }
            other => panic!("an escaped toplevel must be Destroyed, got {other:?}"),
        }
    }

    /// The healthy case: toplevel == root → Intact.
    #[test]
    fn classify_matching_toplevel_is_intact() {
        let root = PathBuf::from("/managed/base/.worktrees/wt");
        assert_eq!(
            classify(Some(&root), &TopLevel::Resolved(root.clone())),
            WorktreeIntegrity::Intact
        );
    }

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
            path: PathBuf::from("/managed/base/.worktrees/wt"),
            verdict,
        }
    }

    /// A Destroyed finding produces an ERROR roll-up.
    ///
    /// Why: ERROR is the only level the daemon's bug-capture layer persists to
    /// `errors.jsonl` / `list_recent_errors`. A roll-up at any lower level
    /// would be invisible to every operator surface.
    /// Test: this function IS the test.
    #[test]
    fn rollup_reports_destroyed_sessions() {
        let levels = levels_emitted_by(|| {
            alarm_rollup(&[finding(WorktreeIntegrity::Destroyed("gone".into()))]);
        });
        assert!(
            levels.contains(&tracing::Level::ERROR),
            "a destroyed worktree must roll up at ERROR; got {levels:?}"
        );
    }

    /// An Unknown-only audit emits NO roll-up.
    ///
    /// Why: escalating "git was unavailable" to ERROR trains operators to
    /// ignore the alarm, which converts a loud alarm into a silent one — the
    /// exact failure this issue exists to end. Unknown stays a per-finding
    /// warn.
    /// Test: this function IS the test.
    #[test]
    fn rollup_is_silent_for_unknown_only() {
        let levels = levels_emitted_by(|| {
            alarm_rollup(&[finding(WorktreeIntegrity::Unknown("no git".into()))]);
        });
        assert!(
            levels.is_empty(),
            "an Unknown-only audit must not roll up at all; got {levels:?}"
        );
    }

    /// A probe that could not run is Unknown — NEVER Intact.
    ///
    /// Why: "the check didn't run" reported as "the check passed" is the exact
    /// fail-open shape this whole issue is about. An unobservable result is
    /// never a passing result.
    /// Test: this function IS the test.
    #[test]
    fn classify_unavailable_git_is_unknown_not_intact() {
        let root = PathBuf::from("/managed/base/.worktrees/wt");
        let verdict = classify(Some(&root), &TopLevel::Unavailable("no such file".into()));
        assert!(
            matches!(verdict, WorktreeIntegrity::Unknown(_)),
            "an unrunnable probe must be Unknown, never Intact; got {verdict:?}"
        );
        assert_ne!(verdict, WorktreeIntegrity::Intact);
    }
}
