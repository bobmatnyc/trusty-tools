//! Report/outcome types, `--dry-run` preview, and human-summary rendering
//! for `tctl install` — split out of `install.rs` to keep that file under
//! the 500-line production cap (CLAUDE.md / `scripts/check_line_cap.sh`);
//! mirrors the `commands/up/report.rs` split for the same command family.
//!
//! Why: `install.rs`'s core (`run`, `install_all`, `install_one`) and this
//! module's report-shaping/rendering are cohesive but separate concerns —
//! one drives the install, the other describes and prints its outcome.
//!
//! What: [`InstallOutcome`] / [`InstallReport`] are the real-install
//! `--json` envelope (with the graceful-degrade `required` derivation);
//! [`DryRunMember`] / [`DryRunReport`] mirror that shape for `--dry-run`;
//! [`plans_service_bootstrap`] / [`plans_mpm_supervisor_bootstrap`] are the
//! shared predicates both the dry-run preview and the real install use so
//! neither can drift from the other; [`print_dry_run`] and
//! [`print_human_summary`] are the human-readable renderers.
//!
//! Test: `install_tests.rs` (a sibling of `install.rs`, `use super::*;`)
//! covers this module's shaping and derivation logic directly.

use serde::Serialize;

use crate::commands::progress_ui::narrator;
use crate::commands::stable_set::{ManageStrategy, StableMember};
use crate::commands::verify_tail::VerifyTailReport;
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
/// reported success. `required` (graceful-degrade, demo-critical fix) mirrors
/// `StableMember::required`: an OPTIONAL member's failure is reported here
/// (`ok: false`) but never flips [`InstallReport::all_ok`] / the exit code.
/// Test: `tests::report_serialises`, `tests::report_all_ok_reflects_service_failure`,
/// `tests::report_all_ok_reflects_shadow_failure`,
/// `tests::report_all_ok_ignores_optional_failure`.
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
    /// Whether this member is REQUIRED for a verified overall run (mirrors
    /// `StableMember::required`). Drives [`InstallReport::build`]'s `all_ok`
    /// derivation and the human summary's skip-vs-fail wording.
    pub required: bool,
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
    pub(super) fn service_not_attempted() -> (bool, String) {
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
    /// Whether every GATING member installed AND every attempted service
    /// bootstrap on a gating member succeeded (see [`InstallOutcome`]'s
    /// field docs for what counts as a service failure) AND, when the verify
    /// tail ran, it reported verified.
    ///
    /// The gating set is the REQUIRED members when the selection has any —
    /// an OPTIONAL member's failure never flips this (graceful-degrade,
    /// demo-critical fix) — and EVERY selected member when it has none
    /// (#5806; see [`InstallReport::build`]).
    pub all_ok: bool,
    /// The post-install verify-tail result (#2560): `ensure` + health (with
    /// the #2498 kickstart retry). `None` when `--no-verify` skipped it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<VerifyTailReport>,
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
    /// `all_ok = every REQUIRED outcome has ok == true AND service_ok == true
    /// AND shadow_ok == true` (graceful-degrade, demo-critical fix: an
    /// OPTIONAL member is excluded from this check entirely — its failure is
    /// visible in `members` but never fails the run).
    /// #5806 — the graceful-degrade filter used to fail OPEN on an
    /// all-OPTIONAL selection. `filter(required).all(…)` over an empty
    /// iterator is vacuously `true`, so `tctl install tga` reported
    /// `all_ok: true` and exit 0 when tga's install had genuinely failed —
    /// nothing installed, success reported. Adding trusty-installer (also
    /// OPTIONAL) would have made `tctl install trusty-installer` a fourth way
    /// in. The rule now lives once, in
    /// [`crate::commands::stable_set::required_gate`], because the verify
    /// tail wrote the same expression and kept failing open after this one was
    /// fixed.
    ///
    /// An EMPTY member list is `all_ok: false`. `run` returns early on an empty
    /// selection so nothing reaches here today, but "installed nothing" is not
    /// evidence of a successful install, and the vacuous `.all()` that made it
    /// `true` is the same shape being fixed one line up.
    ///
    /// Test: `tests::report_all_ok`, `tests::report_all_ok_reflects_service_failure`,
    /// `tests::report_all_ok_reflects_shadow_failure`,
    /// `tests::report_all_ok_ignores_optional_failure`,
    /// `tests::all_optional_selection_does_not_fail_open`,
    /// `tests::lone_optional_installer_failure_exits_nonzero`,
    /// `tests::empty_report_is_not_all_ok`.
    pub(super) fn build(members: Vec<InstallOutcome>) -> Self {
        let all_ok = !members.is_empty()
            && crate::commands::stable_set::required_gate(
                &members,
                |m| m.required,
                |m| m.ok && m.service_ok && m.shadow_ok,
            );
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
    pub(super) fn with_verify(mut self, verify: VerifyTailReport) -> Self {
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
    pub(super) fn exit_code(&self) -> i32 {
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
    /// Binary name that would land on PATH (the one probed for health).
    pub binary: String,
    /// EVERY binary this member would place, from the shared
    /// `trusty_common::bin_resolve` table (#5805).
    ///
    /// Why: `binary` names one file, but trusty-installer places
    /// `trusty-installer` AND `tctl`, trusty-mpm places `tm` AND `trusty-mpm`,
    /// and trusty-search places `trusty-search` AND `trusty-embedderd`. A
    /// preview that lists one of two is a preview that under-reports the blast
    /// radius on the exact members where it matters most.
    pub binaries: Vec<String>,
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
    /// The directory every binary above would be written to (#5805).
    ///
    /// Why: the preview claimed to show the blast radius but never named the
    /// destination, so an operator could not tell whether an install would
    /// land in the canonical `$CARGO_HOME/bin` (#5777) or somewhere a stale
    /// copy earlier on PATH would keep shadowing. It comes from
    /// [`crate::download::install_dir_or_fallback`] — the same call
    /// `install_one` makes — so the preview cannot name a directory the
    /// install would not use.
    pub install_dir: String,
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
pub(super) fn plans_service_bootstrap(m: &StableMember, service_enabled: bool) -> bool {
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
pub(super) fn plans_mpm_supervisor_bootstrap(m: &StableMember, service_enabled: bool) -> bool {
    service_enabled && m.crate_name == "trusty-mpm"
}

/// Build the `--dry-run` preview report from the resolved member set.
///
/// Why: Extracted so the report's shaping is unit-testable without stdout.
/// What: Maps each [`StableMember`] to a [`DryRunMember`], computing
/// `service_bootstrap` via the shared [`plans_service_bootstrap`] predicate so
/// the preview can never drift from what `install_all` actually does, and
/// listing every binary the member places via [`StableMember::binaries`].
/// `install_dir` is passed in rather than resolved here so the shaping stays
/// pure and the caller uses the SAME resolution `install_one` does (#5805).
/// Test: `tests::dry_run_report_shape`,
/// `tests::dry_run_names_the_canonical_install_dir`,
/// `tests::dry_run_lists_both_installer_binaries`.
pub(super) fn build_dry_run_report(
    selected: &[StableMember],
    service_enabled: bool,
    install_dir: &std::path::Path,
) -> DryRunReport {
    let members = selected
        .iter()
        .map(|m| DryRunMember {
            member: m.crate_name.clone(),
            binary: m.binary.clone(),
            binaries: m.binaries(),
            service_bootstrap: plans_service_bootstrap(m, service_enabled),
        })
        .collect();
    DryRunReport {
        command: "install",
        dry_run: true,
        members,
        service_bootstrap_enabled: service_enabled,
        install_dir: install_dir.display().to_string(),
    }
}

/// Print the `--dry-run` preview and return the process exit code.
///
/// Why: A dry run never fails on its own account — it only reports what WOULD
/// happen; a resolution error (unknown member) is caught earlier in `run`.
/// What: resolves the install destination exactly as `install_one` does
/// ([`crate::download::install_dir_or_fallback`], #5805), then `--json` emits
/// [`DryRunReport`] via `render_json`; otherwise prints a human blast-radius
/// summary matching the wording the confirmation prompt used, naming the
/// destination directory and, per member, every binary it would place there.
/// Returns `0` unless the `--json` write itself fails, in which case `1`
/// (mirrors the real install path's failed-JSON-write handling below).
/// Test: Side-effect-only (stdout); the data it prints is covered by
/// `tests::dry_run_report_shape`,
/// `tests::dry_run_names_the_canonical_install_dir`.
pub(super) fn print_dry_run(selected: &[StableMember], service_enabled: bool, json: bool) -> i32 {
    let install_dir = crate::download::install_dir_or_fallback();
    let report = build_dry_run_report(selected, service_enabled, &install_dir);
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
    eprintln!("tctl install: destination: {}", report.install_dir);
    for m in &report.members {
        let svc = if m.service_bootstrap {
            "would bootstrap launchd service"
        } else {
            "no service bootstrap"
        };
        // #5805: every binary, not just the health-probe one — trusty-installer
        // places `trusty-installer` AND `tctl`.
        eprintln!(
            "tctl install:   {} -> {} ({svc})",
            m.member,
            m.binaries.join(", ")
        );
    }
    eprintln!("tctl install: dry run complete — no changes made.");
    0
}

/// The human summary footer's content, before it reaches stdout.
///
/// Why (#5806): the footer used to be computed inline inside a function that
/// only printed, so nothing could assert it agreed with the exit code — and it
/// did not. Splitting the shaping out is what makes the agreement testable.
/// What: `headline` is the one-line count; `errors` are failures the exit code
/// gates on; `skipped` are non-gating optional gaps.
/// Test: `tests::summary_lines_match_the_gating_set`,
/// `tests::all_optional_failure_summary_does_not_read_as_success`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SummaryLines {
    /// `installed N/M …` — counted over the gating set.
    pub headline: String,
    /// Error-level lines: a gating member that failed to install, or installed
    /// and then failed its service bootstrap.
    pub errors: Vec<String>,
    /// Info-level lines: an OPTIONAL member's failure that does not gate.
    pub skipped: Vec<String>,
}

/// Shape the human summary from the SAME gating set [`InstallReport::build`]
/// derives `all_ok` from.
///
/// Why (#5806): the machine verdict was fixed and the human one was not. For an
/// all-optional selection that failed, this printed `installed 0/0 required
/// component(s)`, an info-level "skipped", and a green `VERIFIED` — while
/// `build` exited 2. Two channels describing one run must not disagree, so both
/// now read [`crate::commands::stable_set::required_gate`]'s rule: REQUIRED
/// members when the selection has any, every selected member when it has none.
///
/// What: counts and reports failures over the gating set; only members OUTSIDE
/// it degrade to an informational "skipped". The headline says "required" only
/// when the gating set genuinely is the required subset — with no required
/// member, `0/0 required` was itself misleading.
///
/// # Postconditions
/// - `errors` is empty iff every gating member installed and bootstrapped, which
///   is exactly [`InstallReport::all_ok`] before the verify tail folds in.
/// - A member never appears in both `errors` and `skipped`.
///
/// Test: `tests::summary_lines_match_the_gating_set`,
/// `tests::all_optional_failure_summary_does_not_read_as_success`.
pub(super) fn summary_lines(report: &InstallReport) -> SummaryLines {
    let any_required = report.members.iter().any(|m| m.required);
    let gating: Vec<&InstallOutcome> = report
        .members
        .iter()
        .filter(|m| m.required || !any_required)
        .collect();
    let gating_ok = gating.iter().filter(|m| m.ok).count();
    let noun = if any_required { "required" } else { "selected" };

    let mut errors: Vec<String> = gating
        .iter()
        .filter(|m| !m.ok)
        .map(|m| format!("{}: {}", m.member, m.detail))
        .collect();
    // #2566 review: a binary can install cleanly (`ok: true`) while its service
    // bootstrap genuinely failed — surface that on the human path too, not just
    // fold it silently into the exit code. Gating members only: `all_ok`
    // likewise ignores a non-gating member's service failure.
    errors.extend(
        gating
            .iter()
            .filter(|m| m.ok && !m.service_ok)
            .map(|m| format!("{}: {}", m.member, m.service_detail)),
    );

    SummaryLines {
        headline: format!("installed {gating_ok}/{} {noun} component(s)", gating.len()),
        errors,
        skipped: report
            .members
            .iter()
            .filter(|m| !m.required && any_required && !m.ok)
            .map(|m| format!("{}: skipped (no prebuilt for this platform)", m.member))
            .collect(),
    }
}

/// Print the human-readable install summary footer.
///
/// Why: After the component table, a one-line verdict tells the operator whether
/// everything landed. Graceful-degrade (demo-critical fix): the headline count
/// covers the gating set only, so an optional component with no prebuilt for
/// this platform never reads as a scary partial failure (e.g. the old
/// `installed 4/7`); optional gaps are listed separately as skipped.
/// What: renders [`summary_lines`] — the headline at info level, gating
/// failures at ERROR level, non-gating gaps at info level.
/// Test: Side-effect-only; the content is tested via [`summary_lines`].
pub(super) fn print_human_summary(report: &InstallReport) {
    let narr = narrator(false);
    let lines = summary_lines(report);
    let _ = narr.info(&lines.headline);
    for line in &lines.errors {
        let _ = narr.error(line);
    }
    for line in &lines.skipped {
        let _ = narr.info(line);
    }
}
