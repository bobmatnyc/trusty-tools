//! The actionable outcome a caller gets back alongside a `Session` snapshot
//! (#4351).
//!
//! Why: `task.run` and `session.status` used to hand back only `id`/`status`/
//! `mode` — opaque to an external caller, which then had no way to tell a
//! caller-facing story ("the change landed on branch X, here is the ref, the
//! run passed"). [`TaskResult`] is the additive answer: the same snapshot
//! still ships, with the actionable fields carried beside it.
//! What: [`TaskResultStatus`] classifies the run; [`TaskResult`] carries that
//! status plus `diff_ref`, `branch`, `pr_ref` and `summary`, every one of them
//! nullable so a producer that cannot know a field says so rather than
//! inventing it. `#[non_exhaustive]` keeps later fields additive: construct
//! through [`TaskResult::new`] / [`TaskResult::pending`] and the `with_*`
//! builders, never a struct literal from outside this crate.
//! Test: `tests::status_maps_every_session_status`,
//! `tests::partial_exit_code_is_its_own_status_not_a_failure`,
//! `tests::task_result_round_trips_through_json`,
//! `tests::absent_optional_fields_deserialise_as_none`.

use serde::{Deserialize, Serialize};

use super::model::SessionStatus;

/// Pass/fail classification of a run, wider than a boolean.
///
/// Why: "did it work" has more than two useful answers. A run that exhausted
/// its turn budget after writing a real, on-disk deliverable is NOT a failure
/// — trusty-code's own CLI has spelled that `ExitCode::Partial` (6) since
/// #2265, and collapsing it into `Failed` is exactly how a caller ends up
/// discarding working code (see `crate::run_task::report::ExitCode`'s docs).
/// What: `Pending` for a run that has not reached a terminal state yet;
/// `Success`/`Partial`/`Failed`/`Cancelled`/`DeadlineExceeded` for the
/// terminal ones. The wire form is snake_case, matching `SessionStatus`'s
/// own convention.
/// Test: `tests::status_maps_every_session_status`,
/// `tests::partial_exit_code_is_its_own_status_not_a_failure`,
/// `tests::status_serialises_snake_case`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskResultStatus {
    /// The run has not reached a terminal state yet.
    Pending,
    /// The run reached a natural stop without error.
    Success,
    /// The run stopped early but produced a deliverable — resumable, not dead.
    Partial,
    /// The run errored.
    Failed,
    /// The run was cancelled by a caller.
    Cancelled,
    /// The run's wall-clock deadline elapsed first.
    DeadlineExceeded,
}

impl TaskResultStatus {
    /// The wire string, identical to the serde representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Success => "success",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::DeadlineExceeded => "deadline_exceeded",
        }
    }

    /// Classify a daemon-side [`SessionStatus`].
    ///
    /// Why: the daemon's own producer (`crate::task::executor`) knows the
    /// session's terminal status and nothing about process exit codes; this is
    /// its one mapping point.
    /// What: `Created`/`Running` are not terminal, so they report `Pending`.
    /// `TurnCapExceeded` reports `Partial` — the same "not done, not dead"
    /// reading `crate::cli_client::render::exit_code_for_status` already gives
    /// it.
    /// Test: `tests::status_maps_every_session_status`.
    pub fn from_session_status(status: SessionStatus) -> Self {
        match status {
            SessionStatus::Created | SessionStatus::Running => Self::Pending,
            SessionStatus::Finished => Self::Success,
            SessionStatus::TurnCapExceeded => Self::Partial,
            SessionStatus::Failed => Self::Failed,
            SessionStatus::Cancelled => Self::Cancelled,
            SessionStatus::DeadlineExceeded => Self::DeadlineExceeded,
        }
    }

    /// Classify a `tcode` process exit code (`ExitCode`, #4351).
    ///
    /// Why: a caller that shells out to `tcode run-task` (see
    /// `trusty_agents::tools::pm_bridge_backend::ProcessPmBridge`) reads the
    /// child's exit status, not a `SessionStatus`. Without this mapping the
    /// only honest reading of "non-zero" is "failed", which silently throws
    /// away the `Partial` (6) distinction the exit code exists to draw.
    /// What: `0`/`4` (`Success`/`NoChanges` — the run completed, it simply
    /// changed nothing) report `Success`; `5` reports `DeadlineExceeded`; `6`
    /// reports `Partial`; every other code, including a signal-killed child
    /// with no code at all, reports `Failed`.
    /// Test: `tests::partial_exit_code_is_its_own_status_not_a_failure`,
    /// `tests::exit_code_mapping_covers_every_documented_code`.
    pub fn from_exit_code(code: Option<i32>) -> Self {
        match code {
            Some(0) | Some(4) => Self::Success,
            Some(5) => Self::DeadlineExceeded,
            Some(6) => Self::Partial,
            _ => Self::Failed,
        }
    }
}

/// The actionable result of a task run, carried beside the `Session` snapshot.
///
/// Why: see the module docs — the `ProposalEnvelope` a calling assistant
/// renders needs something concrete ("branch, ref, pass/fail"), and a bare
/// session id is not that.
/// What: every field except `status` is nullable, and a producer that cannot
/// know one leaves it `None` rather than guessing. `diff_ref` is whatever ref
/// names where the change landed (this crate's daemon reports a git tree
/// object SHA); `branch` is the checked-out branch; `pr_ref` is a PR URL or
/// number; `summary` is a one-line user-facing description. `pr_ref` is never
/// populated by this crate's daemon — nothing in it opens a pull request — and
/// exists for a producer downstream that does.
/// Test: `tests::task_result_round_trips_through_json`,
/// `tests::absent_optional_fields_deserialise_as_none`,
/// `tests::builders_set_each_field`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TaskResult {
    /// Pass/fail classification of the run.
    pub status: TaskResultStatus,
    /// Where the change landed — a commit hash, tree SHA, branch name, or path.
    #[serde(default)]
    pub diff_ref: Option<String>,
    /// The branch the run left checked out.
    #[serde(default)]
    pub branch: Option<String>,
    /// A pull-request URL or number, when a producer opened one.
    #[serde(default)]
    pub pr_ref: Option<String>,
    /// One-line user-facing description of what happened.
    #[serde(default)]
    pub summary: Option<String>,
}

impl TaskResult {
    /// A result carrying `status` and nothing else yet.
    ///
    /// Why: `#[non_exhaustive]` deliberately blocks struct-literal
    /// construction outside this crate, so this plus the `with_*` builders are
    /// the whole construction surface — a field added later cannot break a
    /// downstream caller.
    /// What: every optional field starts `None`.
    /// Test: `tests::builders_set_each_field`.
    pub fn new(status: TaskResultStatus) -> Self {
        Self {
            status,
            diff_ref: None,
            branch: None,
            pr_ref: None,
            summary: None,
        }
    }

    /// The result `task.run` returns immediately, before the run has finished.
    ///
    /// Why: `task.run` is asynchronous — it mints the session, spawns the run
    /// and returns. Reporting `Pending` with empty refs is the truthful
    /// snapshot at that instant; the caller polls `session.status` for the
    /// terminal one.
    /// Test: `crate::task::protocol::tests::task_run_response_carries_a_pending_result`.
    pub fn pending() -> Self {
        Self::new(TaskResultStatus::Pending)
    }

    /// Replace the status, keeping every other field.
    ///
    /// Why: a relaying caller learns the outcome from its child's exit code
    /// while the refs come from the child's own reported result — the two
    /// arrive from different places and must be composable.
    /// Test: `tests::builders_set_each_field`.
    pub fn with_status(mut self, status: TaskResultStatus) -> Self {
        self.status = status;
        self
    }

    /// Set `diff_ref`. `None` leaves it absent.
    /// Test: `tests::builders_set_each_field`.
    pub fn with_diff_ref(mut self, diff_ref: Option<String>) -> Self {
        self.diff_ref = diff_ref;
        self
    }

    /// Set `branch`. `None` leaves it absent.
    /// Test: `tests::builders_set_each_field`.
    pub fn with_branch(mut self, branch: Option<String>) -> Self {
        self.branch = branch;
        self
    }

    /// Set `pr_ref`. `None` leaves it absent.
    /// Test: `tests::builders_set_each_field`.
    pub fn with_pr_ref(mut self, pr_ref: Option<String>) -> Self {
        self.pr_ref = pr_ref;
        self
    }

    /// Set `summary`. `None` leaves it absent.
    /// Test: `tests::builders_set_each_field`.
    pub fn with_summary(mut self, summary: Option<String>) -> Self {
        self.summary = summary;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every `SessionStatus` must land on a deliberate classification — the
    /// non-terminal pair on `Pending`, and `TurnCapExceeded` on `Partial`
    /// rather than being absorbed into `Failed`.
    #[test]
    fn status_maps_every_session_status() {
        for (session, expected) in [
            (SessionStatus::Created, TaskResultStatus::Pending),
            (SessionStatus::Running, TaskResultStatus::Pending),
            (SessionStatus::Finished, TaskResultStatus::Success),
            (SessionStatus::TurnCapExceeded, TaskResultStatus::Partial),
            (SessionStatus::Failed, TaskResultStatus::Failed),
            (SessionStatus::Cancelled, TaskResultStatus::Cancelled),
            (
                SessionStatus::DeadlineExceeded,
                TaskResultStatus::DeadlineExceeded,
            ),
        ] {
            assert_eq!(
                TaskResultStatus::from_session_status(session),
                expected,
                "session status {session:?} mapped to the wrong result status"
            );
        }
    }

    /// #4351: exit 6 is `Partial`, NOT `Failed` — the whole point of the code.
    #[test]
    fn partial_exit_code_is_its_own_status_not_a_failure() {
        let status = TaskResultStatus::from_exit_code(Some(6));
        assert_eq!(status, TaskResultStatus::Partial);
        assert_ne!(status, TaskResultStatus::Failed);
    }

    /// Every exit code `crate::run_task::report::ExitCode` documents maps
    /// deliberately, and an absent code (signal-killed child) fails closed.
    #[test]
    fn exit_code_mapping_covers_every_documented_code() {
        for (code, expected) in [
            (Some(0), TaskResultStatus::Success),
            (Some(2), TaskResultStatus::Failed),
            (Some(3), TaskResultStatus::Failed),
            (Some(4), TaskResultStatus::Success),
            (Some(5), TaskResultStatus::DeadlineExceeded),
            (Some(6), TaskResultStatus::Partial),
            (None, TaskResultStatus::Failed),
        ] {
            assert_eq!(
                TaskResultStatus::from_exit_code(code),
                expected,
                "exit code {code:?} mapped to the wrong result status"
            );
        }
    }

    /// `as_str` and the serde representation must not drift apart.
    #[test]
    fn status_serialises_snake_case() {
        for status in [
            TaskResultStatus::Pending,
            TaskResultStatus::Success,
            TaskResultStatus::Partial,
            TaskResultStatus::Failed,
            TaskResultStatus::Cancelled,
            TaskResultStatus::DeadlineExceeded,
        ] {
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                json!(status.as_str())
            );
        }
    }

    /// A fully-populated result survives a JSON round trip unchanged.
    #[test]
    fn task_result_round_trips_through_json() {
        let result = TaskResult::new(TaskResultStatus::Partial)
            .with_diff_ref(Some("abc123".to_string()))
            .with_branch(Some("feat/x".to_string()))
            .with_pr_ref(Some("https://example.invalid/pull/7".to_string()))
            .with_summary(Some("wrote one file".to_string()));

        let round_tripped: TaskResult =
            serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
        assert_eq!(round_tripped, result);
    }

    /// A producer that only knows the status emits a result whose other fields
    /// read back as `None` — and a payload that OMITS them entirely still
    /// deserialises, so a leaner producer stays compatible.
    #[test]
    fn absent_optional_fields_deserialise_as_none() {
        let parsed: TaskResult = serde_json::from_value(json!({ "status": "success" })).unwrap();
        assert_eq!(parsed.status, TaskResultStatus::Success);
        assert_eq!(parsed.diff_ref, None);
        assert_eq!(parsed.branch, None);
        assert_eq!(parsed.pr_ref, None);
        assert_eq!(parsed.summary, None);
    }

    /// Each builder sets exactly its own field and leaves the rest alone.
    #[test]
    fn builders_set_each_field() {
        let base = TaskResult::pending();
        assert_eq!(base.status, TaskResultStatus::Pending);
        assert_eq!(base.diff_ref, None);

        let built = base
            .clone()
            .with_status(TaskResultStatus::Success)
            .with_diff_ref(Some("d".to_string()))
            .with_branch(Some("b".to_string()))
            .with_pr_ref(Some("p".to_string()))
            .with_summary(Some("s".to_string()));

        assert_eq!(built.status, TaskResultStatus::Success);
        assert_eq!(built.diff_ref.as_deref(), Some("d"));
        assert_eq!(built.branch.as_deref(), Some("b"));
        assert_eq!(built.pr_ref.as_deref(), Some("p"));
        assert_eq!(built.summary.as_deref(), Some("s"));
        // The builders consume `self`, so the original is untouched.
        assert_eq!(base.status, TaskResultStatus::Pending);
    }
}
