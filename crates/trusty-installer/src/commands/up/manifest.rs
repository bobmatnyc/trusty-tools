//! Boot manifest — the data-driven member→stage/policy mapping (DOC-12 §2).
//!
//! Why: DOC-12 §2 and acceptance criterion 9 demand that boot ordering and
//! policy be **manifest-driven**, never a hard-coded `match` on tool names. The
//! `tctl up` stage selector reads `boot_stage` / `boot_policy` / `runtime` off
//! each member here, so swapping the orchestrator (claude-mpm → trusty-mpm) or
//! marking a fourth daemon always-on is a data edit, not a code change.
//!
//! What: Defines the `BootStage`, `BootPolicy`, and `Runtime` value types, the
//! `BootMember` record, and `default_manifest()` — the current-stack mapping
//! from DOC-12 §2's "Default member→stage/policy mapping" table — plus
//! `analyze_core_targets()` (DOC-12 §3.3, the boot-analysis target set).
//!
//! Test: `tests::default_manifest_matches_spec_table` asserts every row of the
//! §2 table; `tests::analyze_core_targets_default` asserts the §3.3 default set.

use serde::Serialize;

/// Which boot stage a member belongs to (DOC-12 §2).
///
/// Why: The `tctl up` sequencer groups members into ordered stages; encoding the
/// stage as data lets the sequencer iterate stages without naming any member.
///
/// What: Mirrors the DOC-12 §2 `boot_stage` enum: `core` (always-on substrate),
/// `analysis` (the boot-analysis pass), `console` (the single HTTP front door),
/// `orchestrator` (the opt-in session manager). `Control` is the controller's
/// own slot (`tctl` — it *is* the orchestrator of boot, so it has no stage of
/// its own to start).
///
/// Test: `tests::default_manifest_matches_spec_table` exercises every variant.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BootStage {
    /// Always-on substrate (memory + search) — STAGE 1.
    Core,
    /// The boot-analysis pass (trusty-analyze over the core crates) — STAGE 2.
    Analysis,
    /// The single HTTP front door (trusty-console) — STAGE 3.
    Console,
    /// The opt-in session manager (trusty-mpm) — STAGE 4.
    Orchestrator,
    /// The controller itself; it orchestrates boot rather than being a stage.
    Control,
}

/// Whether/when `tctl up` starts a member (DOC-12 §2).
///
/// Why: The boot policy decides, per member, whether `up` starts it
/// unconditionally, runs it as the analysis pass, starts it only when opted in,
/// or never auto-starts it — again as data, not branching code.
///
/// What: Mirrors the DOC-12 §2 `boot_policy` enum.
///
/// Test: `tests::default_manifest_matches_spec_table`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BootPolicy {
    /// Started unconditionally and (if missing) auto-installed to latest (Q1).
    AlwaysOn,
    /// Run as the boot-analysis pass — background, non-gating content (Q6).
    BootAnalysis,
    /// Started only when the orchestrator stage is opted in (§4); guided if absent.
    OptIn,
    /// Never auto-started by `tctl up`.
    OnDemand,
}

/// An orchestrator member's agent runtime (DOC-12 §2 `runtime` sub-table).
///
/// Why: STAGE 4b (DOC-12 §6) ensures the orchestrator's agent runtime (today
/// Claude Code) is present and at latest. Keeping the runtime kind + verify
/// policy as per-member data means a future orchestrator with a different
/// runtime declares its own ensure-policy with no controller code change.
///
/// What: Carries the runtime `kind` (e.g. `claude-code`), the `binary` to probe
/// (`claude`), the `min_policy` (`latest` | `present`), and an `install_hint`
/// shown when the runtime is absent (DOC-12 §6 guide-don't-install).
///
/// Test: `tests::orchestrator_has_claude_code_runtime`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Runtime {
    /// Runtime kind discriminator (`claude-code`, `tcode`, …).
    pub kind: String,
    /// Binary the runtime launches / that we probe on PATH (`claude`).
    pub binary: String,
    /// Version policy: `latest` (check + prompt-to-upgrade) or `present` (accept).
    pub min_policy: String,
    /// Guidance shown when the runtime binary is absent (never auto-installed).
    pub install_hint: String,
}

/// A single boot member: its id, binary, stage, policy, and optional runtime.
///
/// Why: The sequencer iterates `BootMember`s and acts purely on their declared
/// fields — the load-bearing "zero per-tool verb-dispatch logic" property
/// (DOC-12 acceptance criterion 9).
///
/// What: `id` is the manifest member id (== crate name); `binary` is the
/// installed binary name probed via its DOC-1 contract verbs; `stage`/`policy`
/// drive selection; `runtime` is set only for orchestrator members.
///
/// Test: `tests::default_manifest_matches_spec_table`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BootMember {
    /// Manifest member id (matches the crate name).
    pub id: String,
    /// Installed binary name probed via its contract verbs.
    pub binary: String,
    /// Boot stage this member belongs to.
    pub stage: BootStage,
    /// Boot policy controlling whether/when `up` starts it.
    pub policy: BootPolicy,
    /// Agent runtime to ensure (orchestrator members only).
    pub runtime: Option<Runtime>,
}

impl BootMember {
    /// Construct a non-orchestrator member (no runtime).
    ///
    /// Why: Most members have no agent runtime; a focused constructor keeps the
    /// `default_manifest` table readable.
    ///
    /// What: Builds a `BootMember` with `runtime = None`.
    ///
    /// Test: Exercised by `default_manifest()` and its tests.
    fn daemon(id: &str, binary: &str, stage: BootStage, policy: BootPolicy) -> Self {
        Self {
            id: id.to_owned(),
            binary: binary.to_owned(),
            stage,
            policy,
            runtime: None,
        }
    }
}

/// The current-stack boot manifest (DOC-12 §2 default mapping table).
///
/// Why: This is the single source of truth for what `tctl up` brings up and in
/// what order. It encodes exactly the DOC-12 §2 "Default member→stage/policy
/// mapping" table so the sequencer never names a member directly.
///
/// What: Returns the ordered member list: memory + search (`core`/`always_on`),
/// trusty-analyze (`analysis`/`boot_analysis`), trusty-console
/// (`console`/`always_on`), trusty-installer (`control`), and trusty-mpm
/// (`orchestrator`/`opt_in`, with the `claude-code` runtime sub-table).
///
/// Test: `tests::default_manifest_matches_spec_table`.
pub fn default_manifest() -> Vec<BootMember> {
    vec![
        BootMember::daemon(
            "trusty-memory",
            "trusty-memory",
            BootStage::Core,
            BootPolicy::AlwaysOn,
        ),
        BootMember::daemon(
            "trusty-search",
            "trusty-search",
            BootStage::Core,
            BootPolicy::AlwaysOn,
        ),
        BootMember::daemon(
            "trusty-analyze",
            "trusty-analyze",
            BootStage::Analysis,
            BootPolicy::BootAnalysis,
        ),
        BootMember::daemon(
            "trusty-console",
            "trusty-console",
            BootStage::Console,
            BootPolicy::AlwaysOn,
        ),
        BootMember::daemon(
            "trusty-installer",
            "trusty-installer",
            BootStage::Control,
            BootPolicy::OnDemand,
        ),
        BootMember {
            id: "trusty-mpm".to_owned(),
            binary: "trusty-mpm".to_owned(),
            stage: BootStage::Orchestrator,
            policy: BootPolicy::OptIn,
            runtime: Some(Runtime {
                kind: "claude-code".to_owned(),
                binary: "claude".to_owned(),
                min_policy: "latest".to_owned(),
                install_hint: "npm i -g @anthropic-ai/claude-code".to_owned(),
            }),
        },
    ]
}

/// The default boot-analysis target set (DOC-12 §3.3 "core crates").
///
/// Why: STAGE 2 analyzes the stack's own substrate. DOC-12 §3.3 defines the
/// default as "every `boot_stage = core` member plus `trusty-common`" (the
/// shared lib), widened in §3.3's worked example to include the analyzer and
/// the controller.
///
/// What: Returns the §3.3 worked-example default set: `trusty-common`,
/// `trusty-search`, `trusty-memory`, `trusty-analyze`, `trusty-installer`.
/// The system manifest may override this (handled by the config layer).
///
/// Test: `tests::analyze_core_targets_default`.
pub fn analyze_core_targets() -> Vec<String> {
    vec![
        "trusty-common".to_owned(),
        "trusty-search".to_owned(),
        "trusty-memory".to_owned(),
        "trusty-analyze".to_owned(),
        "trusty-installer".to_owned(),
    ]
}

/// Return the members belonging to a given boot stage, in manifest order.
///
/// Why: The sequencer asks the manifest "which members are in STAGE N?" rather
/// than hard-coding member ids per stage.
///
/// What: Filters `members` to those whose `stage == stage`, preserving order.
///
/// Test: `tests::members_in_stage_filters`.
pub fn members_in_stage(members: &[BootMember], stage: BootStage) -> Vec<&BootMember> {
    members.iter().filter(|m| m.stage == stage).collect()
}
