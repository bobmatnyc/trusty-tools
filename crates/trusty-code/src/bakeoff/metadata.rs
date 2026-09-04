//! `metadata.json` — the retained-evidence schema every bake-off level must
//! carry (#5441).
//!
//! Why: #5441's gate contract requires each retained L1-L3 level to state
//! tokens, cache use, cost, turns, duration, status, and build provenance. The
//! first independent qualification attempt failed on exactly that: the runner
//! kept raw artifacts but recorded neither the candidate commit, the binary
//! hash, the challenge/runner revision, nor the instruction/agent/skill source
//! digests, so nobody could prove which binary produced the numbers. Declaring
//! the schema HERE — in the crate under test, not in the external runner —
//! makes the runner conform to the gate rather than the gate infer the
//! runner's shape.
//! What: [`LevelMetadata`] and its parts, plus [`LevelMetadata::provenance_gaps`],
//! which names every field a run left empty, `"unknown"`, or zero. Deserialization
//! fails CLOSED: an unrecognised `evidence_mode` string becomes
//! [`EvidenceMode::Unknown`], which the preflight rejects exactly as it rejects
//! a declared mock.
//! Test: `bakeoff::tests::metadata_round_trips_the_documented_shape`,
//! `bakeoff::tests::provenance_gaps_names_every_missing_field`,
//! `bakeoff::tests::unknown_evidence_mode_is_not_real`.

use serde::{Deserialize, Serialize};

/// Whether a level's artifacts came from a real model run or a plumbing-only
/// mock.
///
/// Why: #5441 permits offline/mock runs to validate plumbing but states they
/// "do not satisfy the exit gate". A boolean would let an absent field default
/// to the permissive value; an explicit enum with a catch-all makes an
/// unrecognised or missing declaration fail closed.
/// What: `real` and `mock` deserialize from those literals; anything else
/// (including a typo or a future variant) becomes `Unknown`, which
/// [`EvidenceMode::is_real`] reports as not-real.
/// Test: `bakeoff::tests::unknown_evidence_mode_is_not_real`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EvidenceMode {
    /// A real model/provider run against the pinned candidate build.
    Real,
    /// A mocked or offline run — plumbing validation only.
    Mock,
    /// Any value this build does not recognise.
    #[serde(other)]
    Unknown,
}

impl EvidenceMode {
    /// Whether this evidence can satisfy the milestone exit gate.
    ///
    /// Why: single decision point so the preflight never re-spells the
    /// "mock does not count" rule.
    /// What: true only for [`EvidenceMode::Real`].
    /// Test: `bakeoff::tests::unknown_evidence_mode_is_not_real`.
    pub fn is_real(self) -> bool {
        matches!(self, EvidenceMode::Real)
    }
}

/// Which bake-off runner produced the level, and at what revision.
///
/// Why: #5441's preflight must reject "stale runner paths". A path alone
/// cannot establish that: the same path holds a different runner after a pull.
/// Recording path, revision, and whether that checkout was dirty makes the
/// claim checkable — a dirty runner checkout means the recorded revision does
/// not identify what actually ran, which is precisely how the first
/// qualification attempt's evidence was disqualified.
/// What: plain strings plus a `dirty` flag; no path is resolved or touched by
/// this crate, which never runs the runner.
/// Test: `bakeoff::tests::dirty_runner_checkout_is_rejected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunnerProvenance {
    /// Absolute path to the runner script that produced this level.
    pub path: String,
    /// The runner checkout's commit (or tag) at invocation time.
    pub revision: String,
    /// Whether that checkout had uncommitted changes.
    #[serde(default)]
    pub dirty: bool,
}

/// Model, provider, and timeout the level was invoked with.
///
/// Why: #5441 requires one reproducible invocation; a rerun that silently
/// changed model or timeout is a different experiment, not a comparison.
/// What: the three fields that change a run's outcome independently of the
/// candidate build.
/// Test: `bakeoff::tests::provenance_gaps_names_every_missing_field`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Invocation {
    /// Model identifier, e.g. `anthropic/claude-sonnet-4`.
    pub model: String,
    /// Provider identifier, e.g. `openrouter`.
    pub provider: String,
    /// Wall-clock deadline handed to `run-task`, in seconds.
    pub timeout_secs: u64,
}

/// Which `tcode` binary produced the level.
///
/// Why: "results produced by a different Trusty Code build" is one of the five
/// rejections #5441 names. `version`/`commit`/`commit_date` mirror
/// [`crate::build_info::provenance_json`] so the metadata can be cross-checked
/// against the level's own `tcode_report.json` rather than trusted; the binary
/// hash is what survives when the source tree is gone.
/// What: the three report fields plus a SHA-256 of the invoked binary and a
/// `dirty` flag for the candidate checkout.
/// Test: `bakeoff::tests::report_build_mismatch_is_rejected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BuildProvenance {
    /// Crate semver, matching `tcode_report.json`'s `build.version`.
    pub version: String,
    /// Short git commit, matching `tcode_report.json`'s `build.commit`.
    pub commit: String,
    /// Commit date, matching `tcode_report.json`'s `build.commit_date`.
    pub commit_date: String,
    /// SHA-256 of the `tcode` binary that was invoked.
    pub binary_sha256: String,
    /// Whether the candidate checkout had uncommitted changes.
    #[serde(default)]
    pub dirty: bool,
}

/// Digests of the canonical instruction/agent/skill sources the run consumed.
///
/// Why: #5441's R1 note is explicit that the compatibility runner still writes
/// `.claude/agents` and `.claude/skills`, and that R1 "must record the
/// compatibility-source digests it actually used". Recording digests keeps the
/// evidence honest across the #5425/#5426 source convergence without this gate
/// having to know which layout was canonical on the day.
/// What: three opaque digest strings; this crate compares them across levels
/// and never interprets their algorithm.
/// Test: `bakeoff::tests::source_digest_drift_across_levels_is_rejected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SourceDigests {
    /// Digest of the instruction source (e.g. the composed `CLAUDE.md`).
    pub instructions: String,
    /// Digest of the agent catalog the run resolved.
    pub agents: String,
    /// Digest of the skill catalog the run resolved.
    pub skills: String,
}

/// Token counts for one level, mirroring `tcode_report.json`'s `usage` block.
///
/// Why: #5441 asks for tokens AND cache use separately — a cost change driven
/// by a collapsed cache read is a different finding from one driven by more
/// completion tokens.
/// What: the same four counters `run_task::report` renders.
/// Test: `bakeoff::tests::token_regression_beyond_tolerance_needs_a_disposition`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Tokens {
    /// Prompt tokens billed.
    pub prompt: u64,
    /// Completion tokens billed.
    pub completion: u64,
    /// Tokens served from the provider's prompt cache.
    pub cache_read: u64,
    /// Tokens written into the provider's prompt cache.
    pub cache_creation: u64,
}

impl Tokens {
    /// Total billed tokens across all four counters.
    ///
    /// Why: the baseline comparison needs one scalar per metric; four separate
    /// tolerance verdicts on correlated counters produce noise, not signal.
    /// What: saturating sum of the four fields.
    /// Test: `bakeoff::tests::token_regression_beyond_tolerance_needs_a_disposition`.
    pub fn total(self) -> u64 {
        self.prompt
            .saturating_add(self.completion)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_creation)
    }
}

/// What the level's run actually did.
///
/// Why: #5441 requires terminal status, turns, duration, and cost retained per
/// level so the next milestone can compare against them.
/// What: `status` uses `run_task::report`'s own vocabulary (`success`,
/// `partial`, `deadline_exceeded`, …) so it can be cross-checked against the
/// level's `tcode_report.json` rather than trusted.
/// Test: `bakeoff::tests::metadata_status_must_match_the_run_report`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunTelemetry {
    /// Terminal status string, matching `tcode_report.json`'s `status`.
    pub status: String,
    /// PM turns consumed.
    pub turns: u64,
    /// Wall-clock duration of the run.
    pub duration_secs: f64,
    /// Summed USD cost, or `null` when pricing was unavailable.
    pub cost_usd: Option<f64>,
    /// Token counters.
    pub tokens: Tokens,
}

/// The verifier's verdict for one level.
///
/// Why: verifier pass rate is the first thing #5441's comparison names, and a
/// correctness regression blocks milestone closure outright.
/// What: two counters; `checks_total == 0` means the verifier produced no
/// correctness evidence at all, which the preflight treats as mock evidence.
/// Test: `bakeoff::tests::zero_verifier_checks_is_mock_evidence`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct VerifierResult {
    /// Total checks the verifier ran.
    pub checks_total: u32,
    /// Checks that passed.
    pub checks_passed: u32,
}

/// One level's complete `metadata.json`.
///
/// Why: this is the retained-evidence contract #5441's gate reads. Everything
/// the gate decides comes from this document plus the sibling raw artifacts;
/// nothing is inferred from the filesystem or from a live process.
/// What: level number, evidence mode, runner/challenge/build/source provenance,
/// the invocation, run telemetry, and the verifier verdict.
/// Test: `bakeoff::tests::metadata_round_trips_the_documented_shape`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LevelMetadata {
    /// Bake-off level: 1, 2, or 3.
    pub level: u8,
    /// Whether this is real or mock evidence.
    pub evidence_mode: EvidenceMode,
    /// Which runner produced it.
    pub runner: RunnerProvenance,
    /// The challenge repository's revision.
    pub challenge_revision: String,
    /// Model, provider, timeout.
    pub invocation: Invocation,
    /// Which `tcode` binary ran.
    pub build: BuildProvenance,
    /// Digests of the behavior sources the run consumed.
    pub source_digests: SourceDigests,
    /// What the run did.
    pub run: RunTelemetry,
    /// What the verifier concluded.
    pub verifier: VerifierResult,
}

impl LevelMetadata {
    /// Name every provenance field this document left unusable.
    ///
    /// Why: "missing provenance" is one of #5441's five rejections, and a gate
    /// that reports only "provenance incomplete" sends the operator back to
    /// diff two JSON files by hand. Naming each gap makes one rerun enough.
    /// What: returns the dotted field names that are empty, whitespace-only,
    /// the literal `"unknown"` (what [`crate::build_info`] emits outside a git
    /// checkout), or — for `timeout_secs` — zero. An empty vector means every
    /// provenance field carries a usable value.
    /// Test: `bakeoff::tests::provenance_gaps_names_every_missing_field`,
    /// `bakeoff::tests::a_complete_bundle_passes_preflight`.
    pub fn provenance_gaps(&self) -> Vec<String> {
        let mut gaps = Vec::new();
        let mut require = |name: &str, value: &str| {
            if !is_usable(value) {
                gaps.push(name.to_string());
            }
        };
        require("runner.path", &self.runner.path);
        require("runner.revision", &self.runner.revision);
        require("challenge_revision", &self.challenge_revision);
        require("invocation.model", &self.invocation.model);
        require("invocation.provider", &self.invocation.provider);
        require("build.version", &self.build.version);
        require("build.commit", &self.build.commit);
        require("build.commit_date", &self.build.commit_date);
        require("build.binary_sha256", &self.build.binary_sha256);
        require(
            "source_digests.instructions",
            &self.source_digests.instructions,
        );
        require("source_digests.agents", &self.source_digests.agents);
        require("source_digests.skills", &self.source_digests.skills);
        require("run.status", &self.run.status);
        if self.invocation.timeout_secs == 0 {
            gaps.push("invocation.timeout_secs".to_string());
        }
        gaps
    }

    /// The build identity other levels and the pins are compared against.
    ///
    /// Why: four fields compared field-by-field at three call sites drifts;
    /// one canonical rendering does not.
    /// What: `<version>/<commit>/<commit_date>/<binary_sha256>`.
    /// Test: `bakeoff::tests::build_drift_across_levels_is_rejected`.
    pub fn build_identity(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.build.version, self.build.commit, self.build.commit_date, self.build.binary_sha256
        )
    }
}

/// Whether a recorded provenance string actually identifies anything.
///
/// Why: `build_info` emits the literal `"unknown"` rather than an absent key
/// outside a git checkout, so an emptiness test alone would accept it.
/// What: false for empty, whitespace-only, and case-insensitive `"unknown"`.
/// Test: `bakeoff::tests::provenance_gaps_names_every_missing_field`.
fn is_usable(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("unknown")
}
