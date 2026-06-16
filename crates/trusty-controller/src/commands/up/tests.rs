//! Unit tests for the `tctl up` boot orchestrator (DOC-12).
//!
//! Why: The orchestrator's behaviour is the load-bearing contract of DOC-12
//! (gating, idempotency, opt-in, prompt-first upgrade, exit codes). These tests
//! drive every decision branch against in-memory `Runner` / `ClaudeEnv` /
//! `ConsoleLocator` doubles so no real daemon or `claude` binary is needed.
//!
//! What: Covers manifest mapping (§2), analyze-core targets (§3.3), config
//! parse + precedence (§8), member ensure matrix (§3.4), claude-code flow (§6),
//! the lock (§3.4), report exit codes (§5), and the end-to-end sequence (§3/§5).
//!
//! Test: This *is* the test module; `cargo test -p trusty-controller` runs it.

use std::cell::RefCell;

use super::claude_code::{ensure_claude_code, ClaudeEnv, ClaudeOutcome};
use super::config::{load_boot_config_from, AnalyzeMode, BootConfig};
use super::manifest::{
    analyze_core_targets, default_manifest, members_in_stage, BootPolicy, BootStage,
};
use super::member::{ensure_member, EnsureOutcome, MemberHealth, Runner};
use super::policy::{OrchestratorDecision, ResolvedBootPolicy, TriFlag, UpFlags};
use super::report::{StageReport, StageStatus, UpReport};
use super::stages::{run_sequence, ConsoleLocator, OrchestratorDeps};
use super::system_runner::classify_status;

// ── §2 manifest ───────────────────────────────────────────────────────────────

/// Every row of the DOC-12 §2 default mapping table is present and correct.
#[test]
fn default_manifest_matches_spec_table() {
    let m = default_manifest();
    let by = |id: &str| {
        m.iter()
            .find(|x| x.id == id)
            .cloned()
            .expect("member present")
    };

    let mem = by("trusty-memory");
    assert_eq!(mem.stage, BootStage::Core);
    assert_eq!(mem.policy, BootPolicy::AlwaysOn);

    let search = by("trusty-search");
    assert_eq!(search.stage, BootStage::Core);
    assert_eq!(search.policy, BootPolicy::AlwaysOn);

    let an = by("trusty-analyze");
    assert_eq!(an.stage, BootStage::Analysis);
    assert_eq!(an.policy, BootPolicy::BootAnalysis);

    let con = by("trusty-console");
    assert_eq!(con.stage, BootStage::Console);
    assert_eq!(con.policy, BootPolicy::AlwaysOn);

    let ctl = by("trusty-controller");
    assert_eq!(ctl.stage, BootStage::Control);

    let mpm = by("trusty-mpm");
    assert_eq!(mpm.stage, BootStage::Orchestrator);
    assert_eq!(mpm.policy, BootPolicy::OptIn);
}

/// The orchestrator member carries the claude-code runtime sub-table (§2/§6).
#[test]
fn orchestrator_has_claude_code_runtime() {
    let m = default_manifest();
    let mpm = m.iter().find(|x| x.id == "trusty-mpm").unwrap();
    let rt = mpm.runtime.as_ref().expect("runtime present");
    assert_eq!(rt.kind, "claude-code");
    assert_eq!(rt.binary, "claude");
    assert_eq!(rt.min_policy, "latest");
    assert!(rt.install_hint.contains("claude-code"));
}

/// The §3.3 default analyze-core target set matches the worked example.
#[test]
fn analyze_core_targets_default() {
    let t = analyze_core_targets();
    for want in [
        "trusty-common",
        "trusty-search",
        "trusty-memory",
        "trusty-analyze",
        "trusty-controller",
    ] {
        assert!(t.iter().any(|x| x == want), "missing {want}");
    }
}

/// `members_in_stage` filters by stage in manifest order.
#[test]
fn members_in_stage_filters() {
    let m = default_manifest();
    let core = members_in_stage(&m, BootStage::Core);
    assert_eq!(core.len(), 2);
    assert_eq!(core[0].id, "trusty-memory");
    assert_eq!(core[1].id, "trusty-search");
}

// ── §8 config ─────────────────────────────────────────────────────────────────

#[test]
fn analyze_mode_parses() {
    assert_eq!(AnalyzeMode::parse("blocking"), Some(AnalyzeMode::Blocking));
    assert_eq!(
        AnalyzeMode::parse("BACKGROUND"),
        Some(AnalyzeMode::Background)
    );
    assert_eq!(AnalyzeMode::parse("skip"), Some(AnalyzeMode::Skip));
    assert_eq!(AnalyzeMode::parse("nope"), None);
    assert_eq!(AnalyzeMode::default(), AnalyzeMode::Background);
}

#[test]
fn load_absent_is_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml"); // not created
    let cfg = load_boot_config_from(&path).unwrap();
    assert_eq!(cfg, BootConfig::default());
    assert!(!cfg.boot.with_mpm);
}

#[test]
fn load_parses_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(
        &path,
        "boot:\n  with_mpm: true\n  analyze_core: blocking\n  claude_code:\n    policy: present\n    skip_upgrade: true\nanalyze_core_targets:\n  - trusty-mpm\n",
    )
    .unwrap();
    let cfg = load_boot_config_from(&path).unwrap();
    assert!(cfg.boot.with_mpm);
    assert_eq!(cfg.boot.analyze_core, AnalyzeMode::Blocking);
    assert_eq!(cfg.boot.claude_code.policy, "present");
    assert!(cfg.boot.claude_code.skip_upgrade);
    assert_eq!(cfg.analyze_core_targets, vec!["trusty-mpm".to_owned()]);
}

#[test]
fn load_empty_file_is_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(&path, "   \n").unwrap();
    let cfg = load_boot_config_from(&path).unwrap();
    assert_eq!(cfg, BootConfig::default());
}

/// Why: The #1316 `[controller] auto_update_check` flag must parse from the
/// config file so operators can opt into the notify-only update check.
/// What: Writes a config with `controller.auto_update_check: true` and asserts
/// it round-trips.
/// Test: This is the test.
#[test]
fn config_parses_controller() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(&path, "controller:\n  auto_update_check: true\n").unwrap();
    let cfg = load_boot_config_from(&path).unwrap();
    assert!(cfg.controller.auto_update_check);
}

/// Why: The default posture is OFF — no opt-in, no background probing.
/// What: Asserts the default config disables the auto-update check.
/// Test: This is the test.
#[test]
fn controller_default_off() {
    assert!(!BootConfig::default().controller.auto_update_check);
}

// ── §4/§8 policy precedence ─────────────────────────────────────────────────────

fn flags(with: bool, no: bool, tty: bool, yes: bool) -> UpFlags {
    UpFlags {
        with_mpm: TriFlag::from_pair(with, no),
        analyze_core: None,
        skip_claude_upgrade: false,
        wait: false,
        is_tty: tty,
        yes,
    }
}

#[test]
fn with_mpm_flag_precedence() {
    let cfg = BootConfig::default(); // with_mpm = false
                                     // --with-mpm forces On even when config is false.
    let r = ResolvedBootPolicy::resolve(&flags(true, false, true, false), &cfg, vec![]);
    assert_eq!(r.orchestrator, OrchestratorDecision::On);
    // --no-mpm forces Off even when config is true.
    let mut cfg_on = BootConfig::default();
    cfg_on.boot.with_mpm = true;
    let r = ResolvedBootPolicy::resolve(&flags(false, true, true, false), &cfg_on, vec![]);
    assert_eq!(r.orchestrator, OrchestratorDecision::Off);
}

#[test]
fn config_with_mpm_true() {
    let mut cfg = BootConfig::default();
    cfg.boot.with_mpm = true;
    // No flags, config true → On (config beats the prompt/non-tty default).
    let r = ResolvedBootPolicy::resolve(&flags(false, false, true, false), &cfg, vec![]);
    assert_eq!(r.orchestrator, OrchestratorDecision::On);
}

#[test]
fn tty_unset_prompts() {
    let cfg = BootConfig::default();
    let r = ResolvedBootPolicy::resolve(&flags(false, false, true, false), &cfg, vec![]);
    assert_eq!(r.orchestrator, OrchestratorDecision::Prompt);
}

#[test]
fn non_tty_defaults_off() {
    let cfg = BootConfig::default();
    let r = ResolvedBootPolicy::resolve(&flags(false, false, false, false), &cfg, vec![]);
    assert_eq!(r.orchestrator, OrchestratorDecision::Off);
    // --yes (non-interactive) on a TTY also defaults off (no silent fleet).
    let r = ResolvedBootPolicy::resolve(&flags(false, false, true, true), &cfg, vec![]);
    assert_eq!(r.orchestrator, OrchestratorDecision::Off);
}

#[test]
fn analyze_mode_flag_over_config() {
    let mut cfg = BootConfig::default();
    cfg.boot.analyze_core = AnalyzeMode::Blocking;
    let mut f = flags(false, false, false, false);
    f.analyze_core = Some(AnalyzeMode::Skip);
    let r = ResolvedBootPolicy::resolve(&f, &cfg, vec![]);
    assert_eq!(r.analyze_core, AnalyzeMode::Skip); // flag wins
}

#[test]
fn analyze_targets_config_override() {
    let cfg = BootConfig {
        analyze_core_targets: vec!["only-this".to_owned()],
        ..Default::default()
    };
    let r = ResolvedBootPolicy::resolve(
        &flags(false, false, false, false),
        &cfg,
        vec!["default".to_owned()],
    );
    assert_eq!(r.analyze_targets, vec!["only-this".to_owned()]);
    // Empty config → fall back to defaults.
    let r = ResolvedBootPolicy::resolve(
        &flags(false, false, false, false),
        &BootConfig::default(),
        vec!["default".to_owned()],
    );
    assert_eq!(r.analyze_targets, vec!["default".to_owned()]);
}

#[test]
fn skip_claude_upgrade_flag_or_config() {
    let mut cfg = BootConfig::default();
    cfg.boot.claude_code.skip_upgrade = true;
    let r = ResolvedBootPolicy::resolve(&flags(false, false, false, false), &cfg, vec![]);
    assert!(r.skip_claude_upgrade); // from config
    let mut f = flags(false, false, false, false);
    f.skip_claude_upgrade = true;
    let r = ResolvedBootPolicy::resolve(&f, &BootConfig::default(), vec![]);
    assert!(r.skip_claude_upgrade); // from flag
}

// ── §3.4 member ensure matrix ───────────────────────────────────────────────────

/// A `Runner` double driven by scripted health verdicts and recorded actions.
struct ScriptedRunner {
    probes: RefCell<Vec<MemberHealth>>,
    start_ok: bool,
    install_ok: bool,
    actions: RefCell<Vec<String>>,
}

impl ScriptedRunner {
    fn new(probes: Vec<MemberHealth>) -> Self {
        Self {
            probes: RefCell::new(probes),
            start_ok: true,
            install_ok: true,
            actions: RefCell::new(vec![]),
        }
    }
    fn with_start_failing(mut self) -> Self {
        self.start_ok = false;
        self
    }
    fn with_install_failing(mut self) -> Self {
        self.install_ok = false;
        self
    }
}

impl Runner for ScriptedRunner {
    fn probe(&self, _m: &super::manifest::BootMember) -> MemberHealth {
        let mut p = self.probes.borrow_mut();
        if p.is_empty() {
            MemberHealth::HealthyVersionOk
        } else {
            p.remove(0)
        }
    }
    fn start(&self, _m: &super::manifest::BootMember) -> anyhow::Result<()> {
        self.actions.borrow_mut().push("start".to_owned());
        if self.start_ok {
            Ok(())
        } else {
            anyhow::bail!("scripted start failure")
        }
    }
    fn install(&self, _m: &super::manifest::BootMember) -> anyhow::Result<()> {
        self.actions.borrow_mut().push("install".to_owned());
        if self.install_ok {
            Ok(())
        } else {
            anyhow::bail!("scripted install failure")
        }
    }
}

fn always_on_member() -> super::manifest::BootMember {
    default_manifest()
        .into_iter()
        .find(|m| m.id == "trusty-search")
        .unwrap()
}

fn opt_in_member() -> super::manifest::BootMember {
    default_manifest()
        .into_iter()
        .find(|m| m.id == "trusty-mpm")
        .unwrap()
}

#[test]
fn ensure_healthy_is_noop() {
    let r = ScriptedRunner::new(vec![MemberHealth::HealthyVersionOk]);
    let o: EnsureOutcome = ensure_member(&r, &always_on_member());
    assert!(o.gated_ok);
    assert!(o.action.contains("no-op"));
    assert!(
        r.actions.borrow().is_empty(),
        "healthy member never bounced"
    );
}

#[test]
fn ensure_down_starts() {
    // CHECK=Down, VERIFY=Healthy.
    let r = ScriptedRunner::new(vec![MemberHealth::Down, MemberHealth::HealthyVersionOk]);
    let o = ensure_member(&r, &always_on_member());
    assert!(o.gated_ok);
    assert_eq!(o.action, "started");
    assert_eq!(r.actions.borrow().as_slice(), &["start".to_owned()]);
}

#[test]
fn ensure_missing_always_on_installs() {
    // CHECK=NotInstalled (always-on) → install + start; VERIFY=Healthy.
    let r = ScriptedRunner::new(vec![
        MemberHealth::NotInstalled,
        MemberHealth::HealthyVersionOk,
    ]);
    let o = ensure_member(&r, &always_on_member());
    assert!(o.gated_ok);
    assert_eq!(o.action, "installed+started");
    assert_eq!(
        r.actions.borrow().as_slice(),
        &["install".to_owned(), "start".to_owned()]
    );
}

#[test]
fn ensure_missing_opt_in_guides() {
    // CHECK=NotInstalled (opt-in) → guide, never install.
    let r = ScriptedRunner::new(vec![MemberHealth::NotInstalled]);
    let o = ensure_member(&r, &opt_in_member());
    assert!(!o.gated_ok);
    assert_eq!(o.health, MemberHealth::NotInstalled);
    assert!(o.action.contains("guide"));
    assert!(r.actions.borrow().is_empty(), "opt-in never auto-installed");
}

#[test]
fn ensure_install_failure_reported() {
    let r = ScriptedRunner::new(vec![MemberHealth::NotInstalled]).with_install_failing();
    let o = ensure_member(&r, &always_on_member());
    assert!(!o.gated_ok);
    assert_eq!(o.action, "install-failed");
}

#[test]
fn ensure_start_failure_reported() {
    let r = ScriptedRunner::new(vec![MemberHealth::Down]).with_start_failing();
    let o = ensure_member(&r, &always_on_member());
    assert!(!o.gated_ok);
    assert_eq!(o.action, "start-failed");
}

#[test]
fn classify_status_maps_known_values() {
    assert_eq!(classify_status("healthy"), MemberHealth::HealthyVersionOk);
    assert_eq!(classify_status("OK"), MemberHealth::HealthyVersionOk);
    assert_eq!(classify_status("stale"), MemberHealth::HealthyStale);
    assert_eq!(classify_status("down"), MemberHealth::Down);
    assert_eq!(classify_status("anything-else"), MemberHealth::Down);
}

// ── §6 claude-code flow ─────────────────────────────────────────────────────────

struct ScriptedClaudeEnv {
    present: bool,
    installed: Option<String>,
    latest: Option<String>,
    upgrade_ok: bool,
    confirm: bool,
    consent: bool,
    tty: bool,
    upgraded: RefCell<bool>,
}

impl ScriptedClaudeEnv {
    fn base() -> Self {
        Self {
            present: true,
            installed: Some("1.0.0".to_owned()),
            latest: Some("1.0.0".to_owned()),
            upgrade_ok: true,
            confirm: true,
            consent: true,
            tty: true,
            upgraded: RefCell::new(false),
        }
    }
}

impl ClaudeEnv for ScriptedClaudeEnv {
    fn present(&self) -> bool {
        self.present
    }
    fn installed_version(&self) -> Option<String> {
        self.installed.clone()
    }
    fn latest_version(&self) -> Option<String> {
        self.latest.clone()
    }
    fn upgrade(&self) -> anyhow::Result<()> {
        if self.upgrade_ok {
            *self.upgraded.borrow_mut() = true;
            Ok(())
        } else {
            anyhow::bail!("scripted upgrade failure")
        }
    }
    fn confirm_managed_path(&self) -> bool {
        self.confirm
    }
    fn prompt_upgrade(&self, _i: &str, _l: &str) -> bool {
        self.consent
    }
    fn is_tty(&self) -> bool {
        self.tty
    }
}

fn run_claude(env: &ScriptedClaudeEnv, policy: &str, skip: bool) -> ClaudeOutcome {
    ensure_claude_code(env, "npm i -g @anthropic-ai/claude-code", policy, skip)
}

#[test]
fn claude_absent_guides() {
    let mut e = ScriptedClaudeEnv::base();
    e.present = false;
    let o = run_claude(&e, "latest", false);
    assert!(!o.ready);
    assert_eq!(o.action, "absent-guided");
    assert!(o.detail.contains("claude-code"));
}

#[test]
fn claude_current_confirms_managed_path() {
    let e = ScriptedClaudeEnv::base(); // installed == latest
    let o = run_claude(&e, "latest", false);
    assert!(o.ready);
    assert_eq!(o.action, "current");
    assert!(!*e.upgraded.borrow());
}

#[test]
fn claude_stale_consent_upgrades() {
    let mut e = ScriptedClaudeEnv::base();
    e.latest = Some("2.0.0".to_owned());
    let o = run_claude(&e, "latest", false);
    assert_eq!(o.action, "upgraded");
    assert!(o.ready);
    assert!(*e.upgraded.borrow(), "upgrade applied after consent");
}

#[test]
fn claude_stale_decline_keeps() {
    let mut e = ScriptedClaudeEnv::base();
    e.latest = Some("2.0.0".to_owned());
    e.consent = false;
    let o = run_claude(&e, "latest", false);
    assert_eq!(o.action, "declined");
    assert!(!*e.upgraded.borrow(), "no upgrade without consent");
}

#[test]
fn claude_non_tty_warns_never_applies() {
    let mut e = ScriptedClaudeEnv::base();
    e.latest = Some("2.0.0".to_owned());
    e.tty = false;
    let o = run_claude(&e, "latest", false);
    assert_eq!(o.action, "non-tty-warn");
    assert!(!*e.upgraded.borrow(), "non-tty never upgrades silently");
}

#[test]
fn claude_skip_upgrade_accepts() {
    let mut e = ScriptedClaudeEnv::base();
    e.latest = Some("2.0.0".to_owned());
    let o = run_claude(&e, "latest", true); // skip_upgrade
    assert_eq!(o.action, "skip-upgrade");
    assert!(!*e.upgraded.borrow());
}

#[test]
fn claude_present_policy_accepts() {
    let mut e = ScriptedClaudeEnv::base();
    e.latest = Some("2.0.0".to_owned());
    let o = run_claude(&e, "present", false);
    assert_eq!(o.action, "present-policy");
    assert!(!*e.upgraded.borrow());
}

#[test]
fn claude_upgrade_failure_reported() {
    let mut e = ScriptedClaudeEnv::base();
    e.latest = Some("2.0.0".to_owned());
    e.upgrade_ok = false;
    let o = run_claude(&e, "latest", false);
    assert_eq!(o.action, "upgrade-failed");
}

// ── §3.4 lock ───────────────────────────────────────────────────────────────────

#[test]
fn lock_is_exclusive() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ensure.lock");
    let first = super::lock::BootLock::acquire_at(&path, false).expect("first acquires");
    // Second non-blocking acquire must fail while the first is held.
    assert!(super::lock::BootLock::acquire_at(&path, false).is_err());
    drop(first);
    // After release, a fresh acquire succeeds.
    assert!(super::lock::BootLock::acquire_at(&path, false).is_ok());
}

// ── §5 report exit codes ────────────────────────────────────────────────────────

fn report_with(stages: Vec<(&str, StageStatus)>) -> UpReport {
    let mut r = UpReport::default();
    for (n, s) in stages {
        r.push(StageReport::new(n, s, "x"));
    }
    r
}

#[test]
fn exit_code_clean_is_0() {
    let r = report_with(vec![
        ("core", StageStatus::Ok),
        ("analyze", StageStatus::Ok),
        ("console", StageStatus::Ok),
    ]);
    assert_eq!(r.exit_code(), 0);
    assert_eq!(r.verdict(), "healthy");
}

#[test]
fn exit_code_core_failure_is_1() {
    let r = report_with(vec![
        ("core", StageStatus::Failed),
        ("console", StageStatus::Degraded),
    ]);
    assert_eq!(r.exit_code(), 1);
    assert_eq!(r.verdict(), "failed");
}

#[test]
fn exit_code_console_down_is_2() {
    let r = report_with(vec![
        ("core", StageStatus::Ok),
        ("console", StageStatus::Degraded),
    ]);
    assert_eq!(r.exit_code(), 2);
    assert_eq!(r.verdict(), "degraded");
}

#[test]
fn exit_code_skipped_orchestrator_is_0() {
    let r = report_with(vec![
        ("core", StageStatus::Ok),
        ("console", StageStatus::Ok),
        ("orchestrator", StageStatus::Skipped),
    ]);
    assert_eq!(r.exit_code(), 0);
}

#[test]
fn render_json_is_valid() {
    let r = report_with(vec![("core", StageStatus::Ok)]);
    // Just assert serialisation does not panic and the envelope is well-formed.
    let env = serde_json::json!({
        "stages": r.stages,
        "verdict": r.verdict(),
        "exit_code": r.exit_code(),
    });
    let s = serde_json::to_string(&env).unwrap();
    assert!(s.contains("\"core\""));
}

#[test]
fn render_human_does_not_panic() {
    let mut r = report_with(vec![("core", StageStatus::Ok)]);
    r.console_url = Some("http://127.0.0.1:7777".to_owned());
    r.render(false);
    r.render(true);
}

// ── §3/§5 end-to-end sequence ───────────────────────────────────────────────────

/// A `Runner` that returns a fixed health per member id (by binary).
struct MapRunner {
    /// (member id, health-on-every-probe)
    healths: std::collections::HashMap<String, MemberHealth>,
}

impl MapRunner {
    fn all_healthy() -> Self {
        let mut h = std::collections::HashMap::new();
        for m in default_manifest() {
            h.insert(m.id.clone(), MemberHealth::HealthyVersionOk);
        }
        Self { healths: h }
    }
    fn set(mut self, id: &str, health: MemberHealth) -> Self {
        self.healths.insert(id.to_owned(), health);
        self
    }
}

impl Runner for MapRunner {
    fn probe(&self, m: &super::manifest::BootMember) -> MemberHealth {
        *self
            .healths
            .get(&m.id)
            .unwrap_or(&MemberHealth::HealthyVersionOk)
    }
    fn start(&self, _m: &super::manifest::BootMember) -> anyhow::Result<()> {
        Ok(())
    }
    fn install(&self, _m: &super::manifest::BootMember) -> anyhow::Result<()> {
        Ok(())
    }
}

struct StubConsole(Option<String>);
impl ConsoleLocator for StubConsole {
    fn console_url(&self) -> Option<String> {
        self.0.clone()
    }
}

fn policy_for(orch: OrchestratorDecision, analyze: AnalyzeMode) -> ResolvedBootPolicy {
    ResolvedBootPolicy {
        orchestrator: orch,
        analyze_core: analyze,
        claude_policy: "latest".to_owned(),
        skip_claude_upgrade: false,
        analyze_targets: analyze_core_targets(),
        wait: false,
    }
}

fn find<'a>(r: &'a UpReport, name: &str) -> &'a StageReport {
    r.stages
        .iter()
        .find(|s| s.name == name)
        .expect("stage present")
}

#[test]
fn sequence_clean_all_ok() {
    let m = default_manifest();
    let runner = MapRunner::all_healthy();
    let console = StubConsole(Some("http://127.0.0.1:7777".to_owned()));
    let claude = ScriptedClaudeEnv::base();
    let orch = OrchestratorDeps { claude: &claude };
    let policy = policy_for(OrchestratorDecision::Off, AnalyzeMode::Background);

    let report = run_sequence(&m, &policy, &runner, &console, &orch);
    assert_eq!(find(&report, "core").status, StageStatus::Ok);
    assert_eq!(find(&report, "analyze").status, StageStatus::Ok);
    assert_eq!(find(&report, "console").status, StageStatus::Ok);
    assert_eq!(report.console_url.as_deref(), Some("http://127.0.0.1:7777"));
    assert_eq!(find(&report, "orchestrator").status, StageStatus::Skipped);
    assert_eq!(report.exit_code(), 0);
}

#[test]
fn sequence_core_down_skips_orchestrator_but_starts_console() {
    // search down → core stage fails; memory still healthy → console starts.
    let m = default_manifest();
    let runner = MapRunner::all_healthy().set("trusty-search", MemberHealth::Down);
    let console = StubConsole(Some("http://127.0.0.1:7777".to_owned()));
    let claude = ScriptedClaudeEnv::base();
    let orch = OrchestratorDeps { claude: &claude };
    // Even with opt-in ON, a core failure must skip the orchestrator (§5).
    let policy = policy_for(OrchestratorDecision::On, AnalyzeMode::Background);

    let report = run_sequence(&m, &policy, &runner, &console, &orch);
    assert_eq!(find(&report, "core").status, StageStatus::Failed);
    // Console still starts on partial core (memory healthy) — RESOLVED Q2.
    assert_eq!(find(&report, "console").status, StageStatus::Ok);
    // Orchestrator skipped despite opt-in ON.
    assert_eq!(find(&report, "orchestrator").status, StageStatus::Skipped);
    assert_eq!(report.exit_code(), 1);
}

#[test]
fn sequence_console_skipped_when_no_core_healthy() {
    let m = default_manifest();
    let runner = MapRunner::all_healthy()
        .set("trusty-search", MemberHealth::Down)
        .set("trusty-memory", MemberHealth::Down);
    let console = StubConsole(None);
    let claude = ScriptedClaudeEnv::base();
    let orch = OrchestratorDeps { claude: &claude };
    let policy = policy_for(OrchestratorDecision::Off, AnalyzeMode::Background);

    let report = run_sequence(&m, &policy, &runner, &console, &orch);
    assert_eq!(find(&report, "core").status, StageStatus::Failed);
    assert_eq!(find(&report, "console").status, StageStatus::Skipped);
    assert_eq!(report.exit_code(), 1);
}

#[test]
fn sequence_optin_on_ensures_runtime() {
    let m = default_manifest();
    let runner = MapRunner::all_healthy();
    let console = StubConsole(Some("http://127.0.0.1:7777".to_owned()));
    let claude = ScriptedClaudeEnv::base(); // current → ready
    let orch = OrchestratorDeps { claude: &claude };
    let policy = policy_for(OrchestratorDecision::On, AnalyzeMode::Background);

    let report = run_sequence(&m, &policy, &runner, &console, &orch);
    let o = find(&report, "orchestrator");
    assert_eq!(o.status, StageStatus::Ok);
    assert!(o.detail.contains("claude-code"));
}

#[test]
fn sequence_optin_on_runtime_absent_degrades() {
    let m = default_manifest();
    let runner = MapRunner::all_healthy();
    let console = StubConsole(Some("http://127.0.0.1:7777".to_owned()));
    let mut claude = ScriptedClaudeEnv::base();
    claude.present = false; // claude absent → guide → not ready
    let orch = OrchestratorDeps { claude: &claude };
    let policy = policy_for(OrchestratorDecision::On, AnalyzeMode::Background);

    let report = run_sequence(&m, &policy, &runner, &console, &orch);
    let o = find(&report, "orchestrator");
    // Runtime miss degrades the stage (exit 2) but does not fail the whole boot.
    assert_eq!(o.status, StageStatus::Degraded);
    assert_eq!(report.exit_code(), 2);
}

#[test]
fn sequence_analyze_skip() {
    let m = default_manifest();
    let runner = MapRunner::all_healthy();
    let console = StubConsole(Some("http://127.0.0.1:7777".to_owned()));
    let claude = ScriptedClaudeEnv::base();
    let orch = OrchestratorDeps { claude: &claude };
    let policy = policy_for(OrchestratorDecision::Off, AnalyzeMode::Skip);

    let report = run_sequence(&m, &policy, &runner, &console, &orch);
    assert_eq!(find(&report, "analyze").status, StageStatus::Skipped);
}

#[test]
fn sequence_analyze_daemon_down_degrades_not_fails() {
    let m = default_manifest();
    let runner = MapRunner::all_healthy().set("trusty-analyze", MemberHealth::Down);
    let console = StubConsole(Some("http://127.0.0.1:7777".to_owned()));
    let claude = ScriptedClaudeEnv::base();
    let orch = OrchestratorDeps { claude: &claude };
    let policy = policy_for(OrchestratorDecision::Off, AnalyzeMode::Background);

    let report = run_sequence(&m, &policy, &runner, &console, &orch);
    assert_eq!(find(&report, "analyze").status, StageStatus::Degraded);
    // Core healthy, analyze degraded → overall degraded (exit 2), not failed.
    assert_eq!(report.exit_code(), 2);
}

// ── os_env URL parse ─────────────────────────────────────────────────────────────

#[test]
fn console_url_parses() {
    let url = super::os_env::parse_console_port(br#"{"addr":"127.0.0.1","port":7777}"#);
    assert_eq!(url.as_deref(), Some("http://127.0.0.1:7777"));
    // addr-with-port form is used verbatim.
    let url = super::os_env::parse_console_port(br#"{"addr":"0.0.0.0:9000","port":9000}"#);
    assert_eq!(url.as_deref(), Some("http://0.0.0.0:9000"));
    // missing port → None.
    assert!(super::os_env::parse_console_port(br#"{"addr":"x"}"#).is_none());
}
