//! Boot report + exit-code model (DOC-12 §5 / DOC-4 §7).
//!
//! Why: `tctl up` must render the DOC-4 tools×scope matrix as its final report
//! and exit with a code a boot script can branch on: `0` clean, `2` degraded
//! (e.g. console down but CLI usable / orchestrator runtime miss), `1` hard
//! failure (always-on core down). DOC-12 §5 pins which conditions map to which
//! code; this module is the single place that mapping lives.
//!
//! What: `StageStatus` / `StageReport` capture one stage's verdict; `UpReport`
//! aggregates them, computes the overall verdict + `exit_code()`, and renders
//! either a human matrix or `--json`.
//!
//! Test: `super::tests::exit_code_*` assert each §5 mapping; `render` is
//! exercised for both modes.

use serde::Serialize;

/// A single boot stage's outcome.
///
/// Why: The §5 gating needs a per-stage verdict so the overall exit code can be
/// computed from the stages' worst outcome with the §5 special cases applied.
///
/// What: `Ok` (stage gate passed), `Degraded` (stage usable-but-imperfect, e.g.
/// console down, stale runtime), `Failed` (gate failed — core down), `Skipped`
/// (opt-out stage not run; not a failure).
///
/// Test: `super::tests::exit_code_*`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    /// Stage gate passed.
    Ok,
    /// Stage ran but is imperfect (degraded, non-hard-failing).
    Degraded,
    /// Stage gate failed (hard).
    Failed,
    /// Stage was deliberately not run (opt-out / skip); not a failure.
    Skipped,
}

/// One line of the boot report.
///
/// Why: The human matrix and the JSON envelope both render a list of these.
///
/// What: `name` is the stage label; `status` its verdict; `detail` a human
/// one-liner (member actions, URL, remediation hint).
///
/// Test: `super::tests` aggregate cases.
#[derive(Clone, Debug, Serialize)]
pub struct StageReport {
    /// Stage label, e.g. `core`, `analyze`, `console`, `orchestrator`.
    pub name: String,
    /// Stage verdict.
    pub status: StageStatus,
    /// Human detail line.
    pub detail: String,
}

impl StageReport {
    /// Construct a stage report line.
    ///
    /// Why: Terse constructor for the orchestrator's many `push` sites.
    /// What: Builds a `StageReport` from owned/borrowed parts.
    /// Test: Used throughout `super::tests`.
    pub fn new(name: &str, status: StageStatus, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            status,
            detail: detail.into(),
        }
    }
}

/// The aggregate `tctl up` boot report.
///
/// Why: The orchestrator accumulates stage reports into this and hands it to the
/// renderer; `exit_code()` is the one place the §5 code mapping lives.
///
/// What: Holds the ordered `stages` plus an optional `console_url` (printed when
/// the console came up — the user's web entry point).
///
/// Test: `super::tests::exit_code_*`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct UpReport {
    /// Per-stage verdicts in boot order.
    pub stages: Vec<StageReport>,
    /// The console URL, when STAGE 3 brought it up.
    pub console_url: Option<String>,
}

impl UpReport {
    /// Append a stage report.
    ///
    /// Why: Readable accumulation at the orchestrator call sites.
    /// What: Pushes `stage` onto `stages`.
    /// Test: Used throughout `super::tests`.
    pub fn push(&mut self, stage: StageReport) {
        self.stages.push(stage);
    }

    /// Whether the always-on core stage hard-failed.
    ///
    /// Why: A core failure is the one §5 hard-stop → exit `1`.
    /// What: True if any stage named `core` has status `Failed`.
    /// Test: `super::tests::exit_code_core_failure_is_1`.
    fn core_failed(&self) -> bool {
        self.stages
            .iter()
            .any(|s| s.name == "core" && s.status == StageStatus::Failed)
    }

    /// Compute the process exit code per DOC-12 §5 / DOC-4 §7.
    ///
    /// Why: A boot script branches on this; the mapping must be exact.
    ///
    /// What:
    /// - core failed → `1` (hard stop, dependent stages skipped).
    /// - else any stage `Degraded` or `Failed` (non-core) → `2` (degraded but
    ///   the stack is usable — console down / runtime miss / analyze daemon down).
    /// - else `0` (clean; `Skipped` opt-out stages do not count as failure).
    ///
    /// Test: `super::tests::exit_code_clean_is_0`,
    /// `exit_code_core_failure_is_1`, `exit_code_console_down_is_2`,
    /// `exit_code_skipped_orchestrator_is_0`.
    pub fn exit_code(&self) -> i32 {
        if self.core_failed() {
            return 1;
        }
        let degraded = self
            .stages
            .iter()
            .any(|s| matches!(s.status, StageStatus::Degraded | StageStatus::Failed));
        if degraded {
            2
        } else {
            0
        }
    }

    /// A one-line overall verdict string for the human footer.
    ///
    /// Why: Mirrors DOC-4's "matrix + one-line verdict".
    /// What: Maps the exit code to `healthy` / `degraded` / `failed`.
    /// Test: `super::tests::verdict_matches_exit_code`.
    pub fn verdict(&self) -> &'static str {
        match self.exit_code() {
            0 => "healthy",
            2 => "degraded",
            _ => "failed",
        }
    }

    /// Render the report to stdout (human matrix or `--json`).
    ///
    /// Why: DOC-12's FINAL step renders the DOC-4 matrix + verdict; `--json`
    /// emits the machine envelope a console / CI consumes.
    ///
    /// What: With `json`, serialises `{stages, console_url, verdict, exit_code}`.
    /// Otherwise prints an aligned stage table, the console URL, and the verdict.
    ///
    /// Test: `super::tests::render_json_is_valid`,
    /// `super::tests::render_human_does_not_panic`.
    pub fn render(&self, json: bool) {
        if json {
            let env = serde_json::json!({
                "command": "up",
                "stages": self.stages,
                "console_url": self.console_url,
                "verdict": self.verdict(),
                "exit_code": self.exit_code(),
            });
            println!("{}", serde_json::to_string_pretty(&env).unwrap_or_default());
            return;
        }
        println!("tctl up — boot report");
        for s in &self.stages {
            let mark = match s.status {
                StageStatus::Ok => "ok ",
                StageStatus::Degraded => "deg",
                StageStatus::Failed => "ERR",
                StageStatus::Skipped => "—  ",
            };
            println!("  [{mark}] {:<13} {}", s.name, s.detail);
        }
        if let Some(url) = &self.console_url {
            println!("  console: {url}");
        }
        println!("verdict: {} (exit {})", self.verdict(), self.exit_code());
    }
}
