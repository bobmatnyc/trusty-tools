//! `tm doctor --fix` parks the duplicates `search_project_duplicate_index`
//! flags (#9664).
//!
//! Why: the row named each live worktree, orphaned or same-root duplicate
//! and its remedy, `trusty-search project park <id>`, but `--fix` left them
//! live (owner decision f7, "duplicates are parked"). trusty-search owns the
//! map (f6), so tm never writes it: every park goes through the daemon's
//! `search.project.park` method.
//! What: [`repair_duplicate_indexes`] reads the map and runs the same bounded
//! fan-out the row runs, then parks each flagged duplicate — except the index
//! of the project the doctor runs in. Dry run unless [`RepairMode::Apply`]. A
//! map that was not read refuses the whole run; a group that did not resolve
//! is refused by name; a park that is refused, malformed or unanswered is a
//! failed step, never an applied one.
//! Test: `doctor_search_park_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::Value;

use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};
use crate::daemon::search_rpc;

use super::PROBE_TIMEOUT;
use super::doctor_search_project::{
    DEFAULT_LIMITS, DUPLICATE, FanoutLimits, METHOD_MAP, METHOD_RESOLVE, MapRead, Rpc, call,
    read_map, resolve_groups,
};

/// Park one index (trusty-search B1).
const METHOD_PARK: &str = "search.project.park";

/// One step against the whole run, at the socket.
fn whole_run(socket: &Path, status: StepStatus) -> RepairStep {
    RepairStep {
        check: DUPLICATE,
        path: socket.to_path_buf(),
        what: "park the duplicate trusty-search indexes".to_string(),
        status,
    }
}

/// Park every duplicate the row flags, against the real socket.
///
/// Why: `tm doctor --fix` runs its repairs synchronously, inside the CLI's
/// tokio runtime, so this cannot `block_on` that runtime.
/// What: resolves the socket (an error refuses the run), then drives
/// [`repair_duplicate_indexes_at`] with [`DEFAULT_LIMITS`] on a scoped thread
/// with its own runtime. A runtime that cannot be built, or a worker that
/// panics, is one failed step.
/// Test: `the_blocking_entry_runs_inside_a_runtime`.
pub fn repair_duplicate_indexes(project_dir: &Path, mode: RepairMode) -> Vec<RepairStep> {
    let socket = match search_rpc::search_socket() {
        Ok(socket) => socket,
        Err(e) => {
            return vec![whole_run(
                Path::new("trusty-search.sock"),
                StepStatus::Refused(format!(
                    "cannot resolve the trusty-search socket ({e:#}); nothing parked"
                )),
            )];
        }
    };
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt.block_on(repair_duplicate_indexes_at(
                        &socket,
                        project_dir,
                        DEFAULT_LIMITS,
                        mode,
                    )),
                    Err(e) => vec![whole_run(
                        &socket,
                        StepStatus::Failed(format!("could not build a runtime: {e}")),
                    )],
                }
            })
            .join()
            .unwrap_or_else(|_| {
                vec![whole_run(
                    &socket,
                    StepStatus::Failed("the park worker panicked".to_string()),
                )]
            })
    })
}

/// [`repair_duplicate_indexes`] against an explicit socket and limits.
///
/// What: one `search.project.map` read; anything but a readable map is one
/// refused step and no further call. Then the caller's project is resolved
/// and the pinned groups fanned out exactly as the row does. Each group that
/// did not resolve is a refused step. Each flagged duplicate, once per id, is
/// refused when it is the caller's project, reported as already parked
/// (apply only) when the map lists it, planned in a dry run, and otherwise
/// parked. A second main checkout and a parked duplicate are never flagged,
/// so never touched.
/// Test: `fix_parks_only_the_flagged_duplicates`,
/// `a_dry_run_plans_the_park_and_calls_no_park`,
/// `fix_never_parks_the_callers_own_project`,
/// `fix_refuses_to_park_when_the_map_is_not_read`,
/// `an_unchecked_group_is_refused_not_clean`,
/// `an_already_parked_index_is_reported_not_parked_again`.
pub(super) async fn repair_duplicate_indexes_at(
    socket: &Path,
    project_dir: &Path,
    limits: FanoutLimits,
    mode: RepairMode,
) -> Vec<RepairStep> {
    let refuse = |why: String| vec![whole_run(socket, StepStatus::Refused(why))];
    let (pins, parked) =
        match read_map(call(socket, METHOD_MAP, serde_json::json!({}), PROBE_TIMEOUT).await) {
            MapRead::Readable { pins, parked, .. } => (pins, parked),
            MapRead::Unreadable(msg) => {
                return refuse(format!(
                    "trusty-search cannot read its project map ({msg}); nothing parked"
                ));
            }
            MapRead::Down(e) => {
                return refuse(format!(
                    "trusty-search did not answer at {} ({e}); nothing parked",
                    socket.display()
                ));
            }
            MapRead::NotChecked(why) => return refuse(format!("{why}; nothing parked")),
        };
    let own = OwnProject::resolve(socket, project_dir, limits).await;
    let fanout = resolve_groups(socket, &pins, limits).await;
    let mut steps: Vec<RepairStep> = fanout
        .unchecked
        .iter()
        .map(|(key, why)| RepairStep {
            check: DUPLICATE,
            path: socket.to_path_buf(),
            what: format!("park the duplicates of group `{key}`"),
            status: StepStatus::Refused(format!("not checked: {why}; nothing parked")),
        })
        .collect();
    let mut parker = Parker {
        socket,
        limits,
        started: Instant::now(),
        stop: None,
    };
    let mut seen = BTreeSet::new();
    for group in &fanout.groups {
        for (id, root) in &group.parkable_members {
            if !seen.insert(id.as_str()) {
                continue;
            }
            let at = format!("group `{}` (live `{}`)", group.key, group.answer);
            let mut what = format!("park `{id}`, a duplicate in {at}");
            let status = if own.is(id, root) {
                StepStatus::Refused(format!(
                    "it is the index of the project this doctor runs in ({}); park it by hand \
                     with `trusty-search project park {id}`",
                    own.root.display()
                ))
            } else if parked.contains(id) {
                // #9664: the map parked it after the resolve read; nothing to send.
                if mode == RepairMode::DryRun {
                    continue;
                }
                what = format!("`{id}` in {at} is already parked");
                StepStatus::Applied { backup: None }
            } else if mode == RepairMode::DryRun {
                StepStatus::Planned
            } else {
                parker.park(id).await
            };
            steps.push(RepairStep {
                check: DUPLICATE,
                path: PathBuf::from(root),
                what,
                status,
            });
        }
    }
    steps
}

/// The project the doctor runs in: its root and, when it resolves, its index.
struct OwnProject {
    root: PathBuf,
    index_id: Option<String>,
}

impl OwnProject {
    /// Resolve `project_dir`'s root; a resolve that fails leaves only the root.
    async fn resolve(socket: &Path, project_dir: &Path, limits: FanoutLimits) -> Self {
        let root = trusty_common::resolve_project_root(project_dir);
        let params = serde_json::json!({ "project": root.to_string_lossy() });
        let index_id = match call(socket, METHOD_RESOLVE, params, limits.per_call).await {
            Rpc::Answer(body) => body
                .get("index_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            _ => None,
        };
        Self { root, index_id }
    }

    /// Is the duplicate `id` at `root` this project's own index?
    fn is(&self, id: &str, root: &str) -> bool {
        self.index_id.as_deref() == Some(id) || Path::new(root) == self.root
    }
}

/// The park calls of one run, inside `limits.budget`.
struct Parker<'a> {
    socket: &'a Path,
    limits: FanoutLimits,
    started: Instant,
    /// Set once a park goes unanswered or the budget runs out.
    stop: Option<String>,
}

impl Parker<'_> {
    /// Park `id` and classify the answer.
    ///
    /// What: each call is bounded by `per_call` and what is left of `budget`.
    /// Only an answer echoing `id` with `parked: true` is applied. A refusal
    /// or any other answer is failed with the reason. No answer is failed,
    /// says the park may have been recorded, and stops the run: every later
    /// park is failed as not attempted.
    /// Test: `a_park_that_fails_is_failed_not_fixed`,
    /// `a_park_with_no_answer_is_bounded_and_stops_the_run`.
    async fn park(&mut self, id: &str) -> StepStatus {
        if let Some(reason) = &self.stop {
            return StepStatus::Failed(format!("not attempted: {reason}"));
        }
        let over_budget = format!("past the {:?} budget", self.limits.budget);
        let left = self.limits.budget.saturating_sub(self.started.elapsed());
        if left.is_zero() {
            self.stop = Some(over_budget.clone());
            return StepStatus::Failed(format!("not attempted: {over_budget}"));
        }
        let budget_bound = left < self.limits.per_call;
        let params = serde_json::json!({ "index_id": id });
        let timeout = self.limits.per_call.min(left);
        match call(self.socket, METHOD_PARK, params, timeout).await {
            Rpc::Answer(body) => {
                let parked = body.get("parked").and_then(Value::as_bool) == Some(true);
                let same = body.get("index_id").and_then(Value::as_str) == Some(id);
                if parked && same {
                    StepStatus::Applied { backup: None }
                } else {
                    StepStatus::Failed(format!(
                        "park answered without `parked: true` for `{id}`: {body}"
                    ))
                }
            }
            Rpc::Refused { code, message, .. } => {
                StepStatus::Failed(format!("park refused ({code}): {message}"))
            }
            Rpc::Unreachable(e) => {
                self.stop = Some(if budget_bound {
                    over_budget
                } else {
                    "the daemon stopped answering".to_string()
                });
                StepStatus::Failed(format!(
                    "no answer within {timeout:?} ({e}); the park may still have been \
                     recorded — re-run `tm doctor` to see"
                ))
            }
        }
    }
}

#[cfg(test)]
#[path = "doctor_search_park_tests.rs"]
mod tests;
