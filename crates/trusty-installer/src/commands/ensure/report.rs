//! `tctl ensure` report types and exit-code derivation.
//!
//! Why: the `--json` envelope and the process exit code are a stable contract CI
//! callers branch on. Keeping the report shaping and the exit-code rules in one
//! tested module means the JSON and the exit code can never disagree, and the
//! exit-code precedence (failure > timeout > ok) lives in exactly one place.
//!
//! What: [`EnsureOutcome`] (one MCP-member `.mcp.json` patch result),
//! [`StageOutcome`] (one project-setup stage result: index-register /
//! palace-create), [`EnsureReport`] (the rollup), and the exit-code rules:
//! `0` ready, `2` a patch/stage hard-failure, `4` `--wait` requested but the
//! readiness wait timed out (deferred).
//!
//! Test: `tests` in this module cover the report shaping, `all_ok` derivation,
//! and every exit-code branch.

use serde::Serialize;

/// Process exit code: every MCP patch and project-setup stage succeeded and (if
/// `--wait`) readiness was reached.
///
/// Why: CI / setup scripts treat `0` as "fully provisioned and ready". Naming
/// the code keeps the three call sites from drifting on a bare integer literal.
/// What: the literal `0`.
/// Test: `tests::exit_code_ok`.
pub const EXIT_OK: i32 = 0;

/// Process exit code: a `.mcp.json` patch or a project-setup stage hard-failed.
///
/// Why: a broken `.mcp.json` or a failed index/palace provision is the most
/// serious outcome and must outrank the deferred-wait signal so callers never
/// read a `4` (retryable) when the project is actually mis-provisioned.
/// What: the literal `2`.
/// Test: `tests::exit_code_failure_outranks_timeout`.
pub const EXIT_FAILURE: i32 = 2;

/// Process exit code: `--wait` was requested but readiness was not reached
/// before the timeout (deferred).
///
/// Why: distinct from `0` so a caller that passed `--wait` can detect it did NOT
/// actually observe readiness — the project was patched/provisioned but the
/// daemons did not report ready in time (retry later). Per #1341 this is
/// "reserved for genuine deferral/timeout".
/// What: the literal `4`.
/// Test: `tests::exit_code_timeout`.
pub const EXIT_WAIT_TIMEOUT: i32 = 4;

/// One MCP-serving member's `.mcp.json` ensure outcome.
///
/// Why: a typed per-member record keeps the `--json` report stable + testable.
/// What: `member` key; `ok` whether the patch succeeded; `changed` whether the
/// entry was added/updated; `detail` a human note (or the error on failure).
/// Test: `tests::report_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct EnsureOutcome {
    /// `mcpServers` key patched.
    pub member: String,
    /// Whether the patch succeeded.
    pub ok: bool,
    /// Whether the entry was actually changed (`false` = already current).
    pub changed: bool,
    /// Human detail / error.
    pub detail: String,
}

/// One project-setup stage outcome (index-register or palace-create).
///
/// Why: the index-register and palace-create stages each succeed, no-op
/// (already provisioned), or fail; a typed record makes that tri-state explicit
/// for both the human render and the `--json` consumer, and keeps idempotency
/// observable (`changed = false` means "already there").
/// What: `stage` name; `ok` whether it succeeded (or was a no-op); `changed`
/// whether it created something new; `detail` human note / error.
/// Test: `tests::report_serialises`, `tests::stage_failure_sets_exit_2`.
#[derive(Clone, Debug, Serialize)]
pub struct StageOutcome {
    /// Stage name (`"index-register"` / `"palace-create"`).
    pub stage: String,
    /// Whether the stage succeeded or was an idempotent no-op.
    pub ok: bool,
    /// Whether the stage created/registered something new (`false` = already
    /// present).
    pub changed: bool,
    /// Human detail / error.
    pub detail: String,
}

/// The aggregate ensure report.
///
/// Why: `--json` consumers want the whole ensure pass in one object; the exit
/// code is derived from it so the two are always consistent.
/// What: the `.mcp.json` per-member outcomes, the project-setup stage outcomes,
/// `all_ok` (every member + stage ok), whether `--wait` was requested, and
/// `ready` (only meaningful when `wait`).
/// Test: `tests::report_serialises`, `tests::exit_code_*`.
#[derive(Clone, Debug, Serialize)]
pub struct EnsureReport {
    /// Fixed command tag.
    pub command: &'static str,
    /// Per-member `.mcp.json` outcomes.
    pub members: Vec<EnsureOutcome>,
    /// Project-setup stage outcomes (index-register, palace-create).
    pub stages: Vec<StageOutcome>,
    /// Whether every patch AND every stage succeeded.
    pub all_ok: bool,
    /// Whether `--wait` was requested.
    pub wait: bool,
    /// Whether readiness was reached. Only meaningful when `wait` is `true`;
    /// `false` with `wait = true` means the readiness poll timed out.
    pub ready: bool,
}

impl EnsureReport {
    /// Build the report and derive `all_ok`.
    ///
    /// Why: one place derives the verdict so the JSON and the exit code agree.
    /// What: `all_ok = every member ok AND every stage ok`.
    /// Test: `tests::report_serialises`, `tests::exit_code_*`.
    pub fn build(
        members: Vec<EnsureOutcome>,
        stages: Vec<StageOutcome>,
        wait: bool,
        ready: bool,
    ) -> Self {
        let all_ok = members.iter().all(|m| m.ok) && stages.iter().all(|s| s.ok);
        Self {
            command: "ensure",
            members,
            stages,
            all_ok,
            wait,
            ready,
        }
    }

    /// Process exit code per #1341.
    ///
    /// Why: CI / setup scripts branch on this. A hard failure (`2`) must take
    /// precedence over the deferred-wait signal (`4`) — a broken `.mcp.json` or
    /// a failed provision is the more serious condition. `4` is distinct from
    /// `0` so a `--wait` caller can detect it did NOT observe readiness.
    /// What: `2` when any member/stage failed; else `4` when `--wait` was
    /// requested but `ready` is `false` (timeout); else `0`.
    /// Test: `tests::exit_code_ok`, `tests::exit_code_failure_outranks_timeout`,
    /// `tests::exit_code_timeout`.
    pub fn exit_code(&self) -> i32 {
        if !self.all_ok {
            EXIT_FAILURE
        } else if self.wait && !self.ready {
            EXIT_WAIT_TIMEOUT
        } else {
            EXIT_OK
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_member() -> EnsureOutcome {
        EnsureOutcome {
            member: "trusty-search".to_owned(),
            ok: true,
            changed: true,
            detail: "added/updated".to_owned(),
        }
    }

    fn ok_stage(name: &str) -> StageOutcome {
        StageOutcome {
            stage: name.to_owned(),
            ok: true,
            changed: false,
            detail: "already provisioned".to_owned(),
        }
    }

    /// Why: the JSON envelope is a contract; pin its shape including the new
    /// `stages`, `wait`, and `ready` fields.
    /// What: builds a report and asserts the key set + nested values.
    /// Test: This is the test.
    #[test]
    fn report_serialises() {
        let r = EnsureReport::build(
            vec![ok_member()],
            vec![ok_stage("index-register"), ok_stage("palace-create")],
            true,
            true,
        );
        let v = serde_json::to_value(&r).expect("serialises");
        assert_eq!(v["command"], "ensure");
        assert_eq!(v["all_ok"], true);
        assert_eq!(v["wait"], true);
        assert_eq!(v["ready"], true);
        assert_eq!(v["members"][0]["changed"], true);
        assert_eq!(v["stages"][0]["stage"], "index-register");
        assert_eq!(v["stages"][1]["stage"], "palace-create");
    }

    /// Why: the happy path — everything ok, no wait — must be exit 0.
    /// What: all-ok members + stages, `wait = false` → exit 0.
    /// Test: This is the test.
    #[test]
    fn exit_code_ok() {
        let r = EnsureReport::build(
            vec![ok_member()],
            vec![ok_stage("index-register")],
            false,
            false,
        );
        assert!(r.all_ok);
        assert_eq!(r.exit_code(), EXIT_OK);
    }

    /// Why: a failed `.mcp.json` patch must set `all_ok = false` and exit 2.
    /// What: a failed member → exit 2.
    /// Test: This is the test.
    #[test]
    fn exit_code_member_failure() {
        let bad = EnsureOutcome {
            member: "trusty-memory".to_owned(),
            ok: false,
            changed: false,
            detail: "permission denied".to_owned(),
        };
        let r = EnsureReport::build(vec![ok_member(), bad], vec![], false, false);
        assert!(!r.all_ok);
        assert_eq!(r.exit_code(), EXIT_FAILURE);
    }

    /// Why: a failed project-setup stage must also set exit 2 (the project is
    /// mis-provisioned, not merely "wait deferred").
    /// What: an all-ok member but a failed stage → exit 2.
    /// Test: This is the test.
    #[test]
    fn stage_failure_sets_exit_2() {
        let bad_stage = StageOutcome {
            stage: "index-register".to_owned(),
            ok: false,
            changed: false,
            detail: "daemon down".to_owned(),
        };
        let r = EnsureReport::build(vec![ok_member()], vec![bad_stage], true, false);
        assert!(!r.all_ok);
        assert_eq!(r.exit_code(), EXIT_FAILURE);
    }

    /// Why: when `--wait` is requested but readiness is not reached, the verb
    /// must return the distinct code 4 (timeout/deferred), not 0.
    /// What: all-ok + `wait = true` + `ready = false` → exit 4.
    /// Test: This is the test.
    #[test]
    fn exit_code_timeout() {
        let r = EnsureReport::build(
            vec![ok_member()],
            vec![ok_stage("palace-create")],
            true,
            false,
        );
        assert!(r.all_ok);
        assert_eq!(r.exit_code(), EXIT_WAIT_TIMEOUT);
    }

    /// Why: a hard failure must outrank the deferred-wait signal — a caller must
    /// never read a retryable `4` when the project is actually broken.
    /// What: a failed member + `wait = true` + `ready = false` → exit 2 (not 4).
    /// Test: This is the test.
    #[test]
    fn exit_code_failure_outranks_timeout() {
        let bad = EnsureOutcome {
            member: "trusty-search".to_owned(),
            ok: false,
            changed: false,
            detail: "permission denied".to_owned(),
        };
        let r = EnsureReport::build(vec![bad], vec![], true, false);
        assert!(!r.all_ok);
        assert_eq!(r.exit_code(), EXIT_FAILURE);
    }

    /// Why: when `--wait` is requested AND readiness is reached, exit 0.
    /// What: all-ok + `wait = true` + `ready = true` → exit 0.
    /// Test: This is the test.
    #[test]
    fn exit_code_wait_ready_is_ok() {
        let r = EnsureReport::build(
            vec![ok_member()],
            vec![ok_stage("index-register")],
            true,
            true,
        );
        assert_eq!(r.exit_code(), EXIT_OK);
    }
}
