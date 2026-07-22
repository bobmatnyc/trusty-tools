//! `tctl install` — install the agreed STABLE trusty tool set (#1316, DOC-8).
//!
//! Why: The one-time entry point that brings a machine to a fully-installed
//! stack. Installs the canonical, topologically-ordered stable set as a unit so
//! the platform comes up coherent rather than drifting member-by-member.
//! Idempotent: an already-installed member is re-run through the prebuilt-first
//! path (Phase 2 / #1760) or `cargo install` (cheap no-op when current) and reported.
//!
//! What: Resolves the requested members against [`super::stable_set`], installs
//! each in order via the prebuilt-first strategy: on a Tier-1 platform a prebuilt
//! tarball is downloaded, SHA-256 verified, and atomically placed into the install
//! directory (`~/.local/bin` by default). On non-Tier-1 platforms (or when the
//! prebuilt download fails) the code falls back to
//! `trusty_common::update::perform_upgrade` (= `cargo install <crate> --locked`),
//! verifying cargo is present first. Each freshly-installed binary is
//! health-gated with `verify_installed_binary`. Progress rows are rendered via
//! `trusty-progress`. Honours `--yes` (non-interactive) and `--json` (machine
//! output). Returns a process exit code: 0 all installed, 2 one or more failed.
//!
//! `--dry-run` (#2112) previews the same blast-radius summary the confirmation
//! prompt shows — members, binaries, and planned service actions — and exits 0
//! without installing anything; it behaves identically in TTY, non-TTY, and
//! `--json` contexts. It always bypasses the interactive component picker for
//! determinism — a no-members `--dry-run` previews the FULL stable set, never
//! a TTY-driven subset (see `run`'s doc). When `--dry-run` is not passed and
//! confirmation cannot be obtained (non-TTY, no `--yes`), `run` refuses to
//! install (see [`super::install_gate::decide_install_gate`]) rather than
//! silently proceeding — the exact gap #2112 reported.
//!
//! Test: `tests` covers the JSON envelope shaping (`build_report`), the
//! unknown-member handling, and the dry-run report shaping; the network /
//! `cargo install` path and the confirmation gate's TTY plumbing are
//! side-effecting / covered by `super::install_gate::tests`.

use std::path::{Path, PathBuf};

use serde::Serialize;
use trusty_progress::{Component, ComponentTracker};

use super::dependency_graph::describe_added;
use super::install_gate::{decide_install_gate, GateInputs, InstallGate};
use super::progress_ui::{narrator, prompt_yes_no};
use super::runtime::block_on;
use super::service_bootstrap::{
    bootstrap_enabled, bootstrap_member_service, BootstrapAction, NO_SERVICE_ENV,
};
use super::shadow_check;
use super::stable_set::{select_members_transitive, ManageStrategy, StableMember};
use crate::output::render_json;

/// One member's install outcome for the `--json` report.
///
/// Why: A typed per-member result keeps the machine output stable and testable.
/// `service_ok` / `service_detail` were added after a review of #2566 found
/// that a genuine `service install` failure was reported ONLY via an info-level
/// narration line — `--json` output claimed `all_ok: true` / exit 0 even when
/// every daemon's launchd bootstrap had failed, which is precisely the failure
/// class #2557 existed because of (a broken daemon that LOOKS installed).
/// What: `member` is the crate name; `ok` whether the binary install +
/// health-gate succeeded; `detail` a human note (e.g. the error on binary
/// failure). `service_ok` is `true` when the post-install launchd service
/// bootstrap was not attempted (bootstrap disabled, non-service member, or a
/// plist already present — none of those are failures) OR when it ran and
/// succeeded; it is `false` ONLY when `service install` was attempted and
/// genuinely errored. `service_detail` carries the human note for whichever
/// case applied. `shadow_ok` / `shadow_detail` (#3554) are the analogous pair
/// for PATH-shadow detection: `shadow_ok` is `false` ONLY when the
/// just-installed binary is provably shadowed by a DIFFERENT, earlier-PATH
/// copy of the same name (see `super::shadow_check`) — a genuine "the new
/// version will not take effect" condition, never silently swallowed into a
/// reported success.
/// Test: `tests::report_serialises`, `tests::report_all_ok_reflects_service_failure`,
/// `tests::report_all_ok_reflects_shadow_failure`.
#[derive(Clone, Debug, Serialize)]
pub struct InstallOutcome {
    /// Crate name installed.
    pub member: String,
    /// Whether install + health-gate succeeded.
    pub ok: bool,
    /// Human detail / error message.
    pub detail: String,
    /// Whether the post-install service bootstrap succeeded or was not
    /// attempted (see field doc above for the false-only-on-genuine-failure
    /// contract).
    pub service_ok: bool,
    /// Human note for the service bootstrap outcome.
    pub service_detail: String,
    /// `false` ONLY when the just-installed binary is shadowed on `$PATH` by
    /// a different, stale copy (#3554); `true` when clear or not applicable.
    pub shadow_ok: bool,
    /// Human note for the shadow-detection outcome (the actionable
    /// `ShadowReport::message`, or empty when `shadow_ok`).
    pub shadow_detail: String,
}

impl InstallOutcome {
    /// Build the `service_ok = true`, empty-detail outcome for a member whose
    /// binary install failed (never reached the service-bootstrap step).
    ///
    /// Why: a binary-install failure and a service-bootstrap failure are
    /// distinct failure classes; a binary that never installed cannot have a
    /// "failed" service bootstrap — centralising this avoids a copy-pasted
    /// `service_ok: true, service_detail: String::new()` at every call site.
    /// What: returns `(true, "")`&nbsp;— not-applicable, not a failure.
    /// Test: exercised via `tests::report_all_ok` (binary failure alone still
    /// yields `all_ok == false` from `ok`, independent of this default).
    fn service_not_attempted() -> (bool, String) {
        (true, String::new())
    }
}

/// The aggregate install report.
///
/// Why: `--json` consumers want the whole rollup in one object with an overall
/// verdict; the human path renders the same data as a component table.
/// What: Holds per-member outcomes and the computed `all_ok`.
/// Test: `tests::report_serialises`, `tests::report_all_ok`,
/// `tests::report_all_ok_reflects_service_failure`.
#[derive(Clone, Debug, Serialize)]
pub struct InstallReport {
    /// Fixed command tag for JSON consumers.
    pub command: &'static str,
    /// Per-member install outcomes in install order.
    pub members: Vec<InstallOutcome>,
    /// Whether every member installed AND every attempted service bootstrap
    /// succeeded (see [`InstallOutcome`]'s field docs for what counts as a
    /// service failure) AND, when the verify tail ran, it reported verified.
    pub all_ok: bool,
    /// The post-install verify-tail result (#2560): `ensure` + health (with
    /// the #2498 kickstart retry). `None` when `--no-verify` skipped it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<super::verify_tail::VerifyTailReport>,
}

impl InstallReport {
    /// Build a report from per-member outcomes.
    ///
    /// Why: Centralises the `all_ok` derivation so the exit code and JSON agree,
    /// and so the report cannot claim success while a genuine service-bootstrap
    /// failure occurred (#2566 review finding) OR a genuine PATH-shadow
    /// condition (#3554) leaves the just-installed binary unreachable from a
    /// plain shell invocation — both are "looks installed, actually not
    /// live" failures the report must never paper over.
    /// What: Sets `command = "install"`, `verify = None`, and
    /// `all_ok = every outcome has ok == true AND service_ok == true AND
    /// shadow_ok == true`.
    /// Test: `tests::report_all_ok`, `tests::report_all_ok_reflects_service_failure`,
    /// `tests::report_all_ok_reflects_shadow_failure`.
    fn build(members: Vec<InstallOutcome>) -> Self {
        let all_ok = members.iter().all(|m| m.ok && m.service_ok && m.shadow_ok);
        Self {
            command: "install",
            members,
            all_ok,
            verify: None,
        }
    }

    /// Fold the post-install verify-tail result into this report (#2560).
    ///
    /// Why: `--json` consumers must see ONE object whose `all_ok` (and hence
    /// exit code) already reflects the verify-tail outcome — a caller must
    /// never read `all_ok: true` while the verify tail reported an unhealthy
    /// stack (the exact "looks installed, actually broken" class #2557 /
    /// #2498 existed because of).
    /// What: attaches `verify`; if it reports `!verified`, flips `all_ok` to
    /// `false` (binary + service success alone does not override a verify
    /// failure).
    /// Test: `tests::with_verify_failure_flips_all_ok`,
    /// `tests::with_verify_success_preserves_all_ok`.
    fn with_verify(mut self, verify: super::verify_tail::VerifyTailReport) -> Self {
        if !verify.verified {
            self.all_ok = false;
        }
        self.verify = Some(verify);
        self
    }

    /// Process exit code: 0 when all installed, 2 when any failed.
    ///
    /// Why: A boot/automation script branches on this.
    /// What: `0` if `all_ok`, else `2` (partial/total failure — stack degraded).
    /// Test: `tests::exit_code_reflects_all_ok`.
    fn exit_code(&self) -> i32 {
        if self.all_ok {
            0
        } else {
            2
        }
    }
}

/// One member's planned (not-yet-applied) install action for `--dry-run`.
///
/// Why: `--dry-run` (#2112) must show the operator the exact blast radius —
/// what `tctl install` would do — without doing it; a typed row keeps that
/// preview consistent with the real [`InstallOutcome`] shape it stands in for.
/// What: `member` / `binary` mirror [`StableMember`]; `service_bootstrap` is
/// `true` when a launchd `service install` would be attempted for this member
/// (service bootstrap enabled AND the member is [`ManageStrategy::Launchd`]).
/// Test: `tests::dry_run_report_shape`.
#[derive(Clone, Debug, Serialize)]
pub struct DryRunMember {
    /// Crate name that would be installed.
    pub member: String,
    /// Binary name that would land on PATH.
    pub binary: String,
    /// Whether a post-install launchd service bootstrap would be attempted.
    pub service_bootstrap: bool,
}

/// The `--dry-run` preview report (#2112).
///
/// Why: Mirrors [`InstallReport`]'s shape (`command`, per-member rows) so
/// `--json` consumers get a familiar, stable envelope, with `dry_run: true`
/// as the discriminator instead of an `all_ok` verdict (nothing was applied).
/// What: `members` lists every member that WOULD be installed, in install
/// order; `service_bootstrap_enabled` is the resolved `--no-service` /
/// `TCTL_NO_SERVICE_BOOTSTRAP` state applied to the preview.
/// Test: `tests::dry_run_report_shape`.
#[derive(Clone, Debug, Serialize)]
pub struct DryRunReport {
    /// Fixed command tag for JSON consumers.
    pub command: &'static str,
    /// Always `true` — discriminates this envelope from [`InstallReport`].
    pub dry_run: bool,
    /// Members that would be installed, in install order.
    pub members: Vec<DryRunMember>,
    /// Whether the post-install service bootstrap step is enabled.
    pub service_bootstrap_enabled: bool,
}

/// Whether an install (real or previewed) would attempt a launchd service
/// bootstrap for `m`.
///
/// Why: The `--dry-run` preview (`build_dry_run_report`) and the real install
/// loop (`install_all`) must never drift on this decision — a preview that
/// disagrees with reality is worse than no preview at all. A single shared
/// predicate is the only way to guarantee that.
/// What: `true` when the post-install service-bootstrap step is enabled AND
/// `m` is a [`ManageStrategy::Launchd`]-managed member.
/// Test: `tests::dry_run_report_shape` (via `build_dry_run_report`);
/// `install_all`'s use is exercised indirectly by the (side-effecting) install
/// path.
fn plans_service_bootstrap(m: &StableMember, service_enabled: bool) -> bool {
    service_enabled && m.manage == ManageStrategy::Launchd
}

/// Whether an install would attempt the trusty-mpm launchd SUPERVISOR
/// bootstrap for `m` (#3527).
///
/// Why: `install_mpm_supervisor()` used to run UNCONDITIONALLY whenever
/// trusty-mpm installed — the one member exempt from the #2556
/// `plans_service_bootstrap` opt-out, because trusty-mpm's
/// [`ManageStrategy::OwnVerb`] makes that Launchd-only predicate always return
/// `false` for it regardless of `service_enabled`. This mirrors
/// `plans_service_bootstrap`'s *intent* (respect `--no-service` /
/// `TCTL_NO_SERVICE_BOOTSTRAP`) for the supervisor specifically, so trusty-mpm
/// is no longer the one member that ignores the opt-out.
/// What: `true` iff `service_enabled` AND `m.crate_name == "trusty-mpm"`.
/// Test: `tests::plans_mpm_supervisor_bootstrap_respects_flag`.
fn plans_mpm_supervisor_bootstrap(m: &StableMember, service_enabled: bool) -> bool {
    service_enabled && m.crate_name == "trusty-mpm"
}

/// Build the `--dry-run` preview report from the resolved member set.
///
/// Why: Extracted so the report's shaping is unit-testable without stdout.
/// What: Maps each [`StableMember`] to a [`DryRunMember`], computing
/// `service_bootstrap` via the shared [`plans_service_bootstrap`] predicate so
/// the preview can never drift from what `install_all` actually does.
/// Test: `tests::dry_run_report_shape`.
fn build_dry_run_report(selected: &[StableMember], service_enabled: bool) -> DryRunReport {
    let members = selected
        .iter()
        .map(|m| DryRunMember {
            member: m.crate_name.clone(),
            binary: m.binary.clone(),
            service_bootstrap: plans_service_bootstrap(m, service_enabled),
        })
        .collect();
    DryRunReport {
        command: "install",
        dry_run: true,
        members,
        service_bootstrap_enabled: service_enabled,
    }
}

/// Print the `--dry-run` preview and return the process exit code.
///
/// Why: A dry run never fails on its own account — it only reports what WOULD
/// happen; a resolution error (unknown member) is caught earlier in `run`.
/// What: `--json` emits [`DryRunReport`] via `render_json`; otherwise prints a
/// human blast-radius summary matching the wording the confirmation prompt
/// used, one line per member noting whether it would bootstrap a service.
/// Returns `0` unless the `--json` write itself fails, in which case `1`
/// (mirrors the real install path's failed-JSON-write handling below).
/// Test: Side-effect-only (stdout); the data it prints is covered by
/// `tests::dry_run_report_shape`.
fn print_dry_run(selected: &[StableMember], service_enabled: bool, json: bool) -> i32 {
    let report = build_dry_run_report(selected, service_enabled);
    if json {
        if render_json(&report).is_err() {
            eprintln!("tctl install: failed to write JSON output");
            return 1;
        }
        return 0;
    }
    let names: Vec<&str> = report.members.iter().map(|m| m.member.as_str()).collect();
    eprintln!(
        "tctl install: DRY RUN — would install {} tool(s): {}",
        report.members.len(),
        names.join(", ")
    );
    for m in &report.members {
        let svc = if m.service_bootstrap {
            "would bootstrap launchd service"
        } else {
            "no service bootstrap"
        };
        eprintln!("tctl install:   {} -> {} ({svc})", m.member, m.binary);
    }
    eprintln!("tctl install: dry run complete — no changes made.");
    0
}

/// Handle `tctl install [<members>…]`.
///
/// Why: Phase-1 entry point for the system install flow (#1316 priority 1).
///
/// What: Resolves `members` against the stable set (empty = all). When no
/// members are given AND stdin is a TTY AND `--json` is not set AND not
/// `--dry-run`, presents the Phase-4 interactive component picker so the
/// operator can choose a subset without needing to know crate names.
/// `--dry-run` (#2112) ALWAYS skips the picker — even on a TTY with no
/// members named — so a no-members preview deterministically covers the FULL
/// stable set rather than a TTY-driven subset; it then prints the
/// blast-radius preview and returns 0 without installing. Otherwise, the
/// pre-install confirmation gate
/// ([`super::install_gate::decide_install_gate`]) either proceeds (`--yes`),
/// prompts (TTY available), or REFUSES to install (non-TTY, no `--yes` — the
/// #2112 default-safe fix: previously this silently proceeded). Installs each
/// member in topological order, rendering progress rows; emits either the
/// human component table + summary or the `--json` [`InstallReport`]. Returns
/// the process exit code.
///
/// `no_service` (the `--no-service` flag) — together with the [`NO_SERVICE_ENV`]
/// environment opt-out — suppresses the Phase-7b launchd service bootstrap
/// (#2556) so operators who manage launchd themselves, or CI, install binaries
/// only.
///
/// `force` (#3527) overrides the trusty-mpm supervisor downgrade guard (see
/// `plist_bootstrap::decide_downgrade`) — it has no effect on any other member.
///
/// `no_verify` (#2560) skips the post-install `ensure` + health verify tail
/// (see `super::verify_tail`); the install itself (binaries + services) is
/// unaffected — only the final verification pass is skipped.
///
/// Test: `tests::run_unknown_member_is_error`, `tests::run_dry_run_exits_zero`,
/// `tests::dry_run_full_set_when_no_members_named`. The non-TTY refusal gate
/// itself is TTY-independent and exhaustively covered by
/// `super::install_gate::tests` (see that module's doc for why a `run()`-level
/// refusal test is unsound — it would block forever on a real terminal). The
/// install path is side-effecting (`cargo install`) and validated manually.
#[allow(clippy::too_many_arguments)]
pub fn run(
    members: &[String],
    yes: bool,
    json: bool,
    no_service: bool,
    dry_run: bool,
    force: bool,
    no_verify: bool,
) -> i32 {
    // Phase 4: interactive component picker.
    // Only activates when: no explicit members, stdin is a TTY, not --json,
    // and not --dry-run (a preview must behave identically regardless of TTY).
    let members_override: Vec<String>;
    let members = if members.is_empty() && !json && !dry_run && super::progress_ui::is_tty() {
        let all = super::stable_set::stable_set();
        match super::picker::prompt_picker(&all) {
            Ok(chosen) => {
                if chosen.is_empty() {
                    eprintln!("tctl install: no components selected — nothing to do.");
                    return 0;
                }
                members_override = chosen.iter().map(|m| m.crate_name.clone()).collect();
                &members_override
            }
            Err(e) => {
                eprintln!("tctl install: {e}");
                return 3;
            }
        }
    } else {
        members
    };

    let resolved = select_members_transitive(members);
    let (selected, unknown) = (resolved.members, resolved.unknown);

    if !unknown.is_empty() {
        let msg = format!("unknown member(s): {}", unknown.join(", "));
        if json {
            if render_json(&serde_json::json!({
                "command": "install",
                "error": msg,
            }))
            .is_err()
            {
                eprintln!("tctl install: failed to write JSON output");
                return 1;
            }
        } else {
            eprintln!("tctl install: {msg}");
        }
        return 3;
    }

    // #2036: surface members pulled in transitively by a runtime dependency
    // (e.g. "adding trusty-memory, trusty-search (required by trusty-mpm)")
    // before the blast-radius confirmation, so the operator knows why the set
    // grew beyond what they typed.
    if !json {
        for line in describe_added(&resolved.added) {
            eprintln!("tctl install: {line}");
        }
    }

    // #2556: whether to bootstrap each daemon member's launchd plist after it
    // installs. Disabled by the `--no-service` flag or the env opt-out.
    // Resolved early (pure, no side effects) so the dry-run preview below can
    // report the same planned service actions the real install would take.
    let service_enabled = bootstrap_enabled(no_service, std::env::var_os(NO_SERVICE_ENV).is_some());

    // #2112: `--dry-run` previews the blast radius and exits — before any
    // prerequisite detection, prompting, or install side effect. Behaves
    // identically in TTY, non-TTY, and `--json` contexts.
    if dry_run {
        return print_dry_run(&selected, service_enabled, json);
    }

    // Phase 6: detect and optionally install external prerequisites.
    // Prints ✓ lines for found tools and offers interactive install for missing ones.
    {
        use super::prereqs::phase::{run_prereq_phase, PrereqPhaseConfig};
        let crate_names: Vec<String> = selected.iter().map(|m| m.crate_name.clone()).collect();
        let phase_result = run_prereq_phase(&PrereqPhaseConfig {
            selected: &crate_names,
            yes,
            json,
        });
        // If trusty-mpm is selected and tmux is still missing after the phase,
        // give the user one final chance to abort the overall install or continue.
        let mpm_selected = selected.iter().any(|m| m.crate_name == "trusty-mpm");
        let tmux_still_missing = phase_result
            .still_missing
            .iter()
            .any(|m| m.binary == "tmux");
        if mpm_selected && tmux_still_missing {
            if !json && super::progress_ui::is_tty() && !yes {
                let q = "trusty-mpm requires tmux (still not installed). Continue install anyway?";
                if !super::progress_ui::prompt_yes_no(q) {
                    eprintln!("tctl install: aborted (tmux not installed).");
                    return 3;
                }
            } else if !json {
                eprintln!(
                    "tctl install: warning: trusty-mpm requires tmux which is not installed; \
                     managed sessions will not work until tmux is available."
                );
            }
        }
    }

    // Confirm the blast radius (#1316 default-safe, hardened by #2112): probe
    // the TTY exactly once and route through the pure gate so the decision is
    // auditable and cannot silently proceed when consent is unobtainable.
    let tty = super::progress_ui::is_tty();
    match decide_install_gate(GateInputs { yes, is_tty: tty }) {
        InstallGate::Proceed => {}
        InstallGate::Refuse => {
            let msg = "refusing to install without confirmation in a non-interactive \
                       context; pass --yes to proceed non-interactively, or --dry-run \
                       to preview what would be installed.";
            if json {
                if render_json(&serde_json::json!({"command": "install", "error": msg})).is_err() {
                    eprintln!("tctl install: failed to write JSON output");
                    return 1;
                }
            } else {
                eprintln!("tctl install: {msg}");
            }
            return 3;
        }
        InstallGate::NeedsPrompt => {
            // `--json` mode never prompts (a human y/n prompt has no place in a
            // machine-readable stream); preserves the pre-#2112 behaviour for
            // the TTY + --json combination, which is unaffected by this fix.
            if !json {
                let names: Vec<&str> = selected.iter().map(|m| m.crate_name.as_str()).collect();
                let q = format!("Install {} tool(s): {}? ", selected.len(), names.join(", "));
                if !prompt_yes_no(&q) {
                    eprintln!("tctl install: aborted (no confirmation).");
                    return 3;
                }
            }
        }
    }

    let mut report = block_on(install_all(&selected, json, service_enabled, force));

    // #2560: post-install verify tail — ensure + health (with the #2498
    // kickstart retry), folded into the SAME report/exit-code so `--json`
    // consumers never see `all_ok: true` while the stack is actually down.
    // Skipped entirely by `--no-verify` (the install itself is unaffected).
    if !no_verify {
        let verify = super::verify_tail::run_verify_tail(&selected);
        report = report.with_verify(verify);
    }

    if json {
        // A failed machine-readable write must not exit 0: automation would
        // read success from the exit code while the JSON never arrived.
        if render_json(&report).is_err() {
            eprintln!("tctl install: failed to write JSON output");
            return 1;
        }
    } else {
        print_human_summary(&report);
        // The verify tail is printed LAST — it is the final, authoritative
        // "did this actually work" signal the operator should see (#2560).
        if let Some(verify) = &report.verify {
            super::verify_tail::print_human(verify);
        }
    }
    report.exit_code()
}

/// A just-installed binary's resolved location + health-gated version
/// (#3554) — [`install_one`]'s return value.
///
/// Why: `install_all` needs the CONCRETE path (never re-derived by name) for
/// two downstream steps that #3554 identified as PATH-shadowable: the
/// trusty-mpm supervisor bootstrap's candidate-version comparison, and the
/// post-install shadow-detection warning. Threading the path through as data
/// (rather than each downstream step re-resolving it) is what makes both
/// immune to PATH order.
struct InstalledBinary {
    /// The concrete path the binary was health-gated at (never a bare name).
    path: PathBuf,
    /// The version that exact binary reported via `--version`.
    version: String,
}

/// Install every selected member in order, returning the aggregate report.
///
/// Why: The async core, separated from the sync CLI shell so the runtime is
/// owned in exactly one place (`run`).
/// What: For each member, runs `perform_upgrade` then health-gates the
/// CONCRETE installed binary (`install_one` / #3554), rendering a
/// `trusty-progress` narration line per member and a final component table of
/// the installed binaries (with on-disk sizes when resolvable). When
/// `service_enabled`, each launchd-managed daemon member also has its own
/// `<binary> service install` run after it lands (#2556, Phase 7b) so the
/// plist exists before `tctl start`. After a successful install, also checks
/// whether the just-installed binary is PATH-shadowed by a stale copy
/// (#3554) and surfaces that loudly, folding a genuine shadow into
/// `all_ok`/the exit code rather than a silent success.
/// Test: Side-effecting; the report shaping is tested via `InstallReport` and the
/// per-member service policy is tested in `super::service_bootstrap::tests`.
async fn install_all(
    selected: &[StableMember],
    json: bool,
    service_enabled: bool,
    force: bool,
) -> InstallReport {
    let narr = narrator(json);
    let _ = narr.info(&format!("installing {} component(s)", selected.len()));

    // Resolve the install directory once for post-install hooks (Phase 7 & 8).
    let install_dir = crate::download::default_install_dir().unwrap_or_else(|| {
        dirs::home_dir()
            .map(|h| h.join(".cargo").join("bin"))
            .unwrap_or_else(|| std::path::PathBuf::from("/usr/local/bin"))
    });

    // #3554: the real $PATH, resolved once, used by every member's
    // shadow-detection check below.
    let path_env = std::env::var_os("PATH").unwrap_or_default();

    let mut outcomes = Vec::with_capacity(selected.len());
    let mut tracker = ComponentTracker::new(narr.output());

    for m in selected {
        let _ = narr.info(&format!("installing {}", m.crate_name));
        match install_one(m).await {
            Ok(installed) => {
                tracker.add(Component::new(m.binary.clone(), binary_size(&m.binary)));
                // Phase 7: bootstrap trusty-mpm supervisor plist (fail-soft).
                // #3527: gated behind the SAME `--no-service` /
                // `TCTL_NO_SERVICE_BOOTSTRAP` decision as every other daemon —
                // previously this ran unconditionally and could tear down /
                // downgrade a live production supervisor regardless of the
                // opt-out. A refusal from the downgrade guard inside
                // `install_mpm_supervisor` also surfaces here as a (non-fatal)
                // `Err`, which correctly leaves the running daemon untouched.
                // #3554: passes `installed.path` — the CONCRETE binary this
                // install just health-gated — as the supervisor's candidate
                // version source, never a PATH-shadowable name lookup.
                if m.crate_name == "trusty-mpm" {
                    if plans_mpm_supervisor_bootstrap(m, service_enabled) {
                        if let Err(e) =
                            super::plist_bootstrap::install_mpm_supervisor(force, &installed.path)
                        {
                            let _ = narr.info(&format!(
                                "warning: trusty-mpm supervisor bootstrap failed (non-fatal): {e}"
                            ));
                        }
                    } else {
                        let _ = narr.info(
                            "trusty-mpm supervisor bootstrap skipped (--no-service / \
                             TCTL_NO_SERVICE_BOOTSTRAP)",
                        );
                    }
                }
                // Phase 8: codesign + FDA guidance for trusty-search (single
                // source of truth: `macos_signing`, unified with
                // `scripts/install-trusty-search-signed.sh` and `tctl sign` — #2558).
                // PR #2657 review: this automatic hook signs WITHOUT Hardened
                // Runtime (matching pre-PR behavior) because trusty-search's
                // bundled ONNX runtime dylib load path under Hardened Runtime
                // is not yet empirically verified — see
                // `macos_signing::use_hardened_runtime`. `tctl sign trusty-search`
                // / the wrapper script (explicit operator action) DO sign with
                // Hardened Runtime, also matching pre-PR behavior.
                if m.crate_name == "trusty-search" {
                    super::macos_signing::post_install_search(&install_dir, json);
                }
                // Phase 8b: codesign + App-Data-TCC guidance for trusty-mpm.
                // Owner-authorized scope extension (#2558, 2026-07-14): `tm`
                // reads other apps' $HOME containers (Claude config dirs, tmux
                // state), so macOS's App Data TCC category re-prompts on every
                // ad-hoc-signed rebuild the same way FDA does for trusty-search;
                // the same stable-identity Developer-ID signing fixes it. Unlike
                // trusty-search, trusty-mpm loads no dylibs, so this automatic
                // hook DOES sign with Hardened Runtime (low risk either way).
                if m.crate_name == "trusty-mpm" {
                    super::macos_signing::post_install_mpm(&install_dir, json);
                }
                // Phase 7b (#2556): bootstrap the launchd plist for each shared
                // daemon (search/memory/analyze/review/console) via its own
                // `<binary> service install`, so `tctl start` has a plist to
                // load. trusty-mpm (OwnVerb, handled above) and tga (non-daemon)
                // are excluded. Fail-soft for the BINARY install phase (the
                // member is still reported `ok: true` — the binary is on PATH
                // and healthy) but the REPORT must not lie: a genuine bootstrap
                // failure is routed through `narr.error()` (not `info()`) and
                // folds into `service_ok` / `all_ok` / the exit code (#2566
                // review — `--json` previously reported `all_ok: true` even
                // when every daemon's service bootstrap had failed).
                let (service_ok, service_detail) = if plans_service_bootstrap(m, service_enabled) {
                    let action = bootstrap_member_service(&m.binary);
                    let note = action.note(&m.binary);
                    match &action {
                        BootstrapAction::Failed(_) => {
                            let _ = narr.error(&note);
                        }
                        BootstrapAction::Skipped(_) | BootstrapAction::Installed => {
                            let _ = narr.info(&note);
                        }
                    }
                    (!matches!(action, BootstrapAction::Failed(_)), note)
                } else {
                    InstallOutcome::service_not_attempted()
                };
                // Phase 9 (#3554): after a successful, health-gated install,
                // check whether a plain shell invocation of `m.binary` would
                // actually run the binary we just installed, or a stale copy
                // shadowing it earlier on $PATH. A genuine shadow is reported
                // LOUDLY (`narr.error`, not `info`) and folds into
                // `shadow_ok`/`all_ok`/the exit code — the install succeeded
                // on disk, but the operator's shell would not see it, which
                // is exactly the #3554 "looks installed, actually isn't
                // live" failure class.
                let (shadow_ok, shadow_detail) = match shadow_check::detect(
                    &m.binary,
                    &installed.path,
                    Some(&installed.version),
                    &path_env,
                )
                .await
                {
                    Some(report) => {
                        let msg = report.message();
                        let _ = narr.error(&format!("{}: {msg}", m.crate_name));
                        (false, msg)
                    }
                    None => (true, String::new()),
                };
                outcomes.push(InstallOutcome {
                    member: m.crate_name.clone(),
                    ok: true,
                    detail: "installed".to_owned(),
                    service_ok,
                    service_detail,
                    shadow_ok,
                    shadow_detail,
                });
            }
            Err(e) => {
                let _ = narr.error(&format!("{}: {e}", m.crate_name));
                let (service_ok, service_detail) = InstallOutcome::service_not_attempted();
                outcomes.push(InstallOutcome {
                    member: m.crate_name.clone(),
                    ok: false,
                    detail: e.to_string(),
                    service_ok,
                    service_detail,
                    shadow_ok: true,
                    shadow_detail: String::new(),
                });
            }
        }
    }

    // The component table (rustup-style) summarising what landed.
    if !json {
        let _ = tracker.print();
    }
    InstallReport::build(outcomes)
}

/// Select the concrete path for `binary` from a prebuilt placement's
/// reported `paths` (#3554 review — MEDIUM).
///
/// Why: this — plus [`cargo_fallback_bin_path`] — is the exact glue that TWO
/// of the three original #3554 bugs lived in (picking the wrong file after
/// an otherwise-correct install/verify split). It cannot be exercised via a
/// real `install_one` call in tests without a network round-trip
/// (`download::try_install_prebuilt` hits GitHub Releases), so it is
/// extracted as a pure function specifically to make it independently
/// unit-testable without one.
/// What: returns the first entry in `paths` whose file name exactly equals
/// `binary`, or `install_dir.join(binary)` when none match (a defensive
/// fallback — `download::try_install_prebuilt` always places the requested
/// crate's binaries under `install_dir`, so this arm should not be reachable
/// in practice, but must never panic if it is).
/// Test: `tests::select_prebuilt_bin_path_matches_by_filename`,
/// `tests::select_prebuilt_bin_path_falls_back_when_no_match`,
/// `tests::select_prebuilt_bin_path_does_not_substring_match`.
fn select_prebuilt_bin_path(paths: &[PathBuf], binary: &str, install_dir: &Path) -> PathBuf {
    paths
        .iter()
        .find(|p| p.file_name().and_then(|f| f.to_str()) == Some(binary))
        .cloned()
        .unwrap_or_else(|| install_dir.join(binary))
}

/// Resolve the concrete `cargo install` destination for `binary` (#3554
/// review — MEDIUM, see [`select_prebuilt_bin_path`]'s doc for why this is
/// extracted as a pure function).
/// What: `cargo_bin_dir().unwrap_or(install_dir).join(binary)`.
/// Test: `tests::cargo_fallback_bin_path_joins_binary_onto_cargo_bin_dir`.
fn cargo_fallback_bin_path(binary: &str, install_dir: &Path) -> PathBuf {
    cargo_bin_dir()
        .unwrap_or_else(|| install_dir.to_owned())
        .join(binary)
}

/// Install + health-gate a single member, prebuilt-first with cargo fallback.
///
/// Why: Prebuilt binaries install in seconds without requiring a Rust toolchain;
/// the cargo path is the universal fallback for unsupported platforms and failures.
///
/// What: Resolves the install directory (prefers `~/.local/bin` to avoid cdhash
/// issues on macOS; falls back to cargo path via `perform_upgrade` when prebuilt
/// fails). Calls `crate::download::try_install_prebuilt`; on `Outcome::Fallback`
/// emits a narration line and delegates to `perform_upgrade`. In BOTH cases,
/// health-gates the CONCRETE just-installed path (#3554) via
/// `trusty_common::update::verify_installed_binary_at_path` — never a
/// name-based lookup a stale earlier-PATH/earlier-priority-directory binary
/// could shadow; the path itself is resolved by the pure
/// [`select_prebuilt_bin_path`] / [`cargo_fallback_bin_path`] helpers above.
/// For the prebuilt path, additionally cross-checks the reported version
/// against the version the download layer resolved
/// (`Outcome::Installed::version`); a mismatch is a HARD error (#3554
/// requirement: never a silent pass) rather than a successful report,
/// because it means the binary being health-gated is provably not the one
/// that was just placed.
///
/// Test: Side-effecting; the fallback routing and shaping are unit-tested in
/// `crate::download::tests`. The #3554 shape (health gate immune to a
/// PATH-shadowing stale binary) is covered at the `verify_installed_binary_at_path`
/// layer (`trusty-common`) and the `shadow_check`/`plist_bootstrap` layers; the
/// path-selection glue specifically is covered by `select_prebuilt_bin_path`'s
/// and `cargo_fallback_bin_path`'s own tests (see above).
async fn install_one(m: &StableMember) -> anyhow::Result<InstalledBinary> {
    use crate::download::{self, Outcome};

    // Resolve the install directory — prefer ~/.local/bin (the default used by
    // install.sh) so the prebuilt binary lands where the user expects; fall back
    // to the cargo-install path when it cannot be determined.
    let install_dir = download::default_install_dir().unwrap_or_else(|| {
        // Fallback: CARGO_HOME/bin or ~/.cargo/bin.
        let cargo_home = std::env::var("CARGO_HOME").unwrap_or_default();
        if cargo_home.is_empty() {
            dirs::home_dir()
                .map(|h| h.join(".cargo").join("bin"))
                .unwrap_or_else(|| std::path::PathBuf::from("/usr/local/bin"))
        } else {
            std::path::PathBuf::from(&cargo_home).join("bin")
        }
    });

    let outcome = download::try_install_prebuilt(&m.crate_name, &install_dir).await;
    match outcome {
        Outcome::Installed { paths, version } => {
            tracing::info!(crate_name = %m.crate_name, %version, "installed from prebuilt");
            // #3554: health-gate the CONCRETE just-placed binary — the exact
            // path `download::try_install_prebuilt` reports having written —
            // never a name re-resolved afterward.
            let bin_path = select_prebuilt_bin_path(&paths, &m.binary, &install_dir);
            let reported = trusty_common::update::verify_installed_binary_at_path(&bin_path)
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "health gate failed for {} at {}: {e}",
                        m.crate_name,
                        bin_path.display()
                    )
                })?;
            // #3554: the health gate passing is not enough on its own — cross
            // check that the exact binary we just probed reports the SAME
            // version the download layer believes it placed. A mismatch here
            // means `bin_path` is provably NOT the binary just installed
            // (e.g. an unexpected pre-existing file at that exact path), and
            // must be a hard error, never a silently "passed" health gate.
            let reported_version = super::update_engine::extract_version_from_line(&reported);
            if reported_version.as_deref() != Some(version.as_str()) {
                anyhow::bail!(
                    "health gate mismatch for {}: installer believes it just installed \
                     version {version} to {}, but that exact binary reports {reported_version:?} \
                     (raw `--version` output: {reported:?}) — refusing to report success",
                    m.crate_name,
                    bin_path.display(),
                );
            }
            Ok(InstalledBinary {
                path: bin_path,
                version,
            })
        }
        Outcome::Fallback { reason } => {
            tracing::info!(crate_name = %m.crate_name, %reason, "prebuilt unavailable; using cargo install");
            // Verify cargo is available before attempting the fallback.
            which::which("cargo").map_err(|_| {
                anyhow::anyhow!(
                    "no Rust toolchain found on PATH (cargo not available); \
                     cannot fall back to `cargo install {}`",
                    m.crate_name
                )
            })?;
            trusty_common::update::perform_upgrade(&m.crate_name).await?;
            // #3554: `cargo install` always lands in the cargo bin dir
            // (`$CARGO_HOME/bin`, falling back to `~/.cargo/bin`) — resolve
            // that CONCRETE destination directly rather than a name lookup.
            let bin_path = cargo_fallback_bin_path(&m.binary, &install_dir);
            let reported = trusty_common::update::verify_installed_binary_at_path(&bin_path)
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "health gate failed for {} at {}: {e}",
                        m.crate_name,
                        bin_path.display()
                    )
                })?;
            let version = super::update_engine::extract_version_from_line(&reported)
                .unwrap_or_else(|| reported.trim().to_owned());
            Ok(InstalledBinary {
                path: bin_path,
                version,
            })
        }
    }
}

/// Best-effort on-disk size of an installed binary (for the component row).
///
/// Why: The rustup-style table shows a size column; we look up the installed
/// file's size, defaulting to 0 when it cannot be resolved (the row still renders).
/// What: Resolves the cargo bin directory (`$CARGO_HOME/bin`, falling back to
/// `~/.cargo/bin`) so the size resolves under a non-default `CARGO_HOME` (CI),
/// then returns `<bin>/<binary>`'s byte length or 0.
/// Test: Side-effect-only (filesystem); `cargo_bin_dir` is unit-tested in
/// `tests::cargo_bin_dir_honours_cargo_home`.
fn binary_size(binary: &str) -> u64 {
    cargo_bin_dir()
        .map(|d| d.join(binary))
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|md| md.len())
        .unwrap_or(0)
}

/// Resolve the cargo binary install directory.
///
/// Why: `cargo install` honours `CARGO_HOME`; hardcoding `~/.cargo/bin` would
/// mis-resolve installed-binary sizes under a custom `CARGO_HOME` (e.g. CI).
/// What: Reads `CARGO_HOME` from the process environment and delegates to the
/// pure [`cargo_bin_dir_from_env`] (which holds the testable resolution rule).
/// Test: `cargo_bin_dir_from_env` is unit-tested directly in
/// `tests::cargo_bin_dir_from_env_*`; this wrapper is the side-effecting shell.
fn cargo_bin_dir() -> Option<std::path::PathBuf> {
    cargo_bin_dir_from_env(std::env::var("CARGO_HOME").ok().as_deref())
}

/// Pure resolution of the cargo bin directory from a `CARGO_HOME` value.
///
/// Why: Extracting the rule from the env read makes it testable WITHOUT mutating
/// the process-global environment, so the test is safe under parallel execution.
/// What: Returns `<cargo_home>/bin` when `cargo_home` is `Some` and non-empty,
/// otherwise `~/.cargo/bin` (or `None` when the home dir cannot be resolved).
/// Test: `tests::cargo_bin_dir_from_env_honours_value`,
/// `tests::cargo_bin_dir_from_env_falls_back`.
fn cargo_bin_dir_from_env(cargo_home: Option<&str>) -> Option<std::path::PathBuf> {
    match cargo_home {
        Some(home) if !home.is_empty() => Some(std::path::PathBuf::from(home).join("bin")),
        _ => dirs::home_dir().map(|h| h.join(".cargo").join("bin")),
    }
}

/// Print the human-readable install summary footer.
///
/// Why: After the component table, a one-line verdict tells the operator whether
/// everything landed.
/// What: Prints `installed N/M` and lists any failures to stderr.
/// Test: Side-effect-only; the data it reads is tested via `InstallReport`.
fn print_human_summary(report: &InstallReport) {
    let ok = report.members.iter().filter(|m| m.ok).count();
    let narr = narrator(false);
    let _ = narr.info(&format!("installed {}/{}", ok, report.members.len()));
    for m in report.members.iter().filter(|m| !m.ok) {
        let _ = narr.error(&format!("{}: {}", m.member, m.detail));
    }
    // #2566 review: a binary can install cleanly (`ok: true`) while its
    // service bootstrap genuinely failed — surface that on the human path too,
    // not just fold it silently into the exit code.
    for m in report.members.iter().filter(|m| m.ok && !m.service_ok) {
        let _ = narr.error(&format!("{}: {}", m.member, m.service_detail));
    }
}

// ── Unit tests ───────────────────────────────────────────────────────────────
// Tests are in a sibling file to keep this definition file under the 500-line
// production cap (CLAUDE.md / scripts/check_line_cap.sh) — mirrors the
// `cli.rs` / `cli_tests.rs` split.
#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
