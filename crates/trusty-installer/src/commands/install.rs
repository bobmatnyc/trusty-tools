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
//! Test: `tests` covers the JSON envelope shaping (`build_report`) and the
//! unknown-member handling; the network / `cargo install` path is side-effecting
//! and validated manually.

use serde::Serialize;
use trusty_progress::{Component, ComponentTracker};

use super::dependency_graph::describe_added;
use super::progress_ui::{narrator, prompt_yes_no};
use super::runtime::block_on;
use super::service_bootstrap::{
    bootstrap_enabled, bootstrap_member_service, BootstrapAction, NO_SERVICE_ENV,
};
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
/// case applied.
/// Test: `tests::report_serialises`, `tests::report_all_ok_reflects_service_failure`.
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
    /// service failure).
    pub all_ok: bool,
}

impl InstallReport {
    /// Build a report from per-member outcomes.
    ///
    /// Why: Centralises the `all_ok` derivation so the exit code and JSON agree,
    /// and so the report cannot claim success while a genuine service-bootstrap
    /// failure occurred (#2566 review finding).
    /// What: Sets `command = "install"` and
    /// `all_ok = every outcome has ok == true AND service_ok == true`.
    /// Test: `tests::report_all_ok`, `tests::report_all_ok_reflects_service_failure`.
    fn build(members: Vec<InstallOutcome>) -> Self {
        let all_ok = members.iter().all(|m| m.ok && m.service_ok);
        Self {
            command: "install",
            members,
            all_ok,
        }
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

/// Handle `tctl install [<members>…]`.
///
/// Why: Phase-1 entry point for the system install flow (#1316 priority 1).
///
/// What: Resolves `members` against the stable set (empty = all). When no
/// members are given AND stdin is a TTY AND `--json` is not set, presents the
/// Phase-4 interactive component picker so the operator can choose a subset
/// without needing to know crate names. On a TTY without `--yes`, confirms the
/// blast radius before installing. Installs each member in topological order,
/// rendering progress rows; emits either the human component table + summary or
/// the `--json` [`InstallReport`]. Returns the process exit code.
///
/// `no_service` (the `--no-service` flag) — together with the [`NO_SERVICE_ENV`]
/// environment opt-out — suppresses the Phase-7b launchd service bootstrap
/// (#2556) so operators who manage launchd themselves, or CI, install binaries
/// only.
///
/// Test: `tests::run_unknown_member_is_error`; the install path itself is
/// side-effecting (`cargo install`) and validated manually.
pub fn run(members: &[String], yes: bool, json: bool, no_service: bool) -> i32 {
    // Phase 4: interactive component picker.
    // Only activates when: no explicit members, stdin is a TTY, and not --json.
    // When members are passed, or in --json / non-TTY mode, behaviour is unchanged.
    let members_override: Vec<String>;
    let members = if members.is_empty() && !json && super::progress_ui::is_tty() {
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

    // Confirm the blast radius unless --yes or non-interactive (#1316 default-safe).
    if !yes && super::progress_ui::is_tty() && !json {
        let names: Vec<&str> = selected.iter().map(|m| m.crate_name.as_str()).collect();
        let q = format!("Install {} tool(s): {}? ", selected.len(), names.join(", "));
        if !prompt_yes_no(&q) {
            eprintln!("tctl install: aborted (no confirmation).");
            return 3;
        }
    }

    // #2556: whether to bootstrap each daemon member's launchd plist after it
    // installs. Disabled by the `--no-service` flag or the env opt-out.
    let service_enabled = bootstrap_enabled(no_service, std::env::var_os(NO_SERVICE_ENV).is_some());

    let report = block_on(install_all(&selected, json, service_enabled));
    if json {
        // A failed machine-readable write must not exit 0: automation would
        // read success from the exit code while the JSON never arrived.
        if render_json(&report).is_err() {
            eprintln!("tctl install: failed to write JSON output");
            return 1;
        }
    } else {
        print_human_summary(&report);
    }
    report.exit_code()
}

/// Install every selected member in order, returning the aggregate report.
///
/// Why: The async core, separated from the sync CLI shell so the runtime is
/// owned in exactly one place (`run`).
/// What: For each member, runs `perform_upgrade` then `verify_installed_binary`,
/// rendering a `trusty-progress` narration line per member and a final component
/// table of the installed binaries (with on-disk sizes when resolvable).
/// When `service_enabled`, each launchd-managed daemon member also has its own
/// `<binary> service install` run after it lands (#2556, Phase 7b) so the plist
/// exists before `tctl start`.
/// Test: Side-effecting; the report shaping is tested via `InstallReport` and the
/// per-member service policy is tested in `super::service_bootstrap::tests`.
async fn install_all(
    selected: &[StableMember],
    json: bool,
    service_enabled: bool,
) -> InstallReport {
    let narr = narrator(json);
    let _ = narr.info(&format!("installing {} component(s)", selected.len()));

    // Resolve the install directory once for post-install hooks (Phase 7 & 8).
    let install_dir = crate::download::default_install_dir().unwrap_or_else(|| {
        dirs::home_dir()
            .map(|h| h.join(".cargo").join("bin"))
            .unwrap_or_else(|| std::path::PathBuf::from("/usr/local/bin"))
    });

    let mut outcomes = Vec::with_capacity(selected.len());
    let mut tracker = ComponentTracker::new(narr.output());

    for m in selected {
        let _ = narr.info(&format!("installing {}", m.crate_name));
        match install_one(m).await {
            Ok(()) => {
                tracker.add(Component::new(m.binary.clone(), binary_size(&m.binary)));
                // Phase 7: bootstrap trusty-mpm supervisor plist (fail-soft).
                if m.crate_name == "trusty-mpm" {
                    if let Err(e) = super::plist_bootstrap::install_mpm_supervisor() {
                        let _ = narr.info(&format!(
                            "warning: trusty-mpm supervisor bootstrap failed (non-fatal): {e}"
                        ));
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
                let (service_ok, service_detail) =
                    if service_enabled && m.manage == ManageStrategy::Launchd {
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
                outcomes.push(InstallOutcome {
                    member: m.crate_name.clone(),
                    ok: true,
                    detail: "installed".to_owned(),
                    service_ok,
                    service_detail,
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

/// Install + health-gate a single member, prebuilt-first with cargo fallback.
///
/// Why: Prebuilt binaries install in seconds without requiring a Rust toolchain;
/// the cargo path is the universal fallback for unsupported platforms and failures.
///
/// What: Resolves the install directory (prefers `~/.local/bin` to avoid cdhash
/// issues on macOS; falls back to cargo path via `perform_upgrade` when prebuilt
/// fails). Calls `crate::download::try_install_prebuilt`; on `Outcome::Fallback`
/// emits a narration line and delegates to `perform_upgrade`. In both cases
/// health-gates with `verify_installed_binary`.
///
/// Test: Side-effecting; the fallback routing and shaping are unit-tested in
/// `crate::download::tests`.
async fn install_one(m: &StableMember) -> anyhow::Result<()> {
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
        Outcome::Installed { version, .. } => {
            tracing::info!(crate_name = %m.crate_name, %version, "installed from prebuilt");
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
        }
    }

    trusty_common::update::verify_installed_binary(&m.binary).await
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an `InstallOutcome` with the pre-#2566 default service state
    /// (not attempted — `service_ok: true`, empty detail), so tests that only
    /// care about the binary-install dimension don't have to repeat the two
    /// new fields at every call site.
    fn outcome(member: &str, ok: bool, detail: &str) -> InstallOutcome {
        InstallOutcome {
            member: member.to_owned(),
            ok,
            detail: detail.to_owned(),
            service_ok: true,
            service_detail: String::new(),
        }
    }

    /// Why: The JSON envelope is a public contract; pin its shape.
    /// What: Builds a report and asserts the serialised keys/values.
    /// Test: This is the test.
    #[test]
    fn report_serialises() {
        let report = InstallReport::build(vec![outcome("trusty-search", true, "installed")]);
        let v = serde_json::to_value(&report).expect("serialises");
        assert_eq!(v["command"], "install");
        assert_eq!(v["all_ok"], true);
        assert_eq!(v["members"][0]["member"], "trusty-search");
        assert_eq!(v["members"][0]["service_ok"], true);
    }

    /// Why: `all_ok` must be false if any member's BINARY install failed.
    /// What: Mixes an ok and a failed outcome; asserts `all_ok = false`.
    /// Test: This is the test.
    #[test]
    fn report_all_ok() {
        let report =
            InstallReport::build(vec![outcome("a", true, ""), outcome("b", false, "boom")]);
        assert!(!report.all_ok);
    }

    /// Why: #2566 review — a binary can install cleanly (`ok: true`) while its
    /// SERVICE bootstrap genuinely fails; the report must not claim
    /// `all_ok: true` in that case (the exact failure class #2557 existed
    /// because of: a "successfully installed" daemon that never actually
    /// works). A `Skipped` service outcome (opt-out, non-service member, or
    /// plist already present) must NOT be treated as a failure.
    /// What: one member with `ok: true, service_ok: false` → `all_ok == false`;
    /// a second report with `ok: true, service_ok: true` (skip case) →
    /// `all_ok == true`.
    /// Test: this is the test.
    #[test]
    fn report_all_ok_reflects_service_failure() {
        let failed = InstallReport::build(vec![InstallOutcome {
            member: "trusty-search".to_owned(),
            ok: true,
            detail: "installed".to_owned(),
            service_ok: false,
            service_detail: "service bootstrap failed (non-fatal): boom".to_owned(),
        }]);
        assert!(
            !failed.all_ok,
            "a genuine service bootstrap failure must flip all_ok to false"
        );
        assert_eq!(failed.exit_code(), 2);

        let skipped = InstallReport::build(vec![InstallOutcome {
            member: "trusty-search".to_owned(),
            ok: true,
            detail: "installed".to_owned(),
            service_ok: true,
            service_detail: "launchd plist already present — left untouched".to_owned(),
        }]);
        assert!(
            skipped.all_ok,
            "a skipped (not failed) service bootstrap must not flip all_ok"
        );
        assert_eq!(skipped.exit_code(), 0);
    }

    /// Why: The exit code must track the verdict for automation.
    /// What: Asserts 0 for all-ok and 2 for a failure.
    /// Test: This is the test.
    #[test]
    fn exit_code_reflects_all_ok() {
        let ok = InstallReport::build(vec![outcome("a", true, "")]);
        assert_eq!(ok.exit_code(), 0);
        let bad = InstallReport::build(vec![outcome("a", false, "x")]);
        assert_eq!(bad.exit_code(), 2);
    }

    /// Why: Re-review fix — `binary_size` must resolve under a non-default
    /// `CARGO_HOME` (CI installs there). The rule lives in the pure
    /// `cargo_bin_dir_from_env`, exercised here WITHOUT mutating the process
    /// environment so the test is safe to run in parallel with any other test.
    /// What: A non-empty value yields `<that>/bin`.
    /// Test: This is the test.
    #[test]
    fn cargo_bin_dir_from_env_honours_value() {
        assert_eq!(
            cargo_bin_dir_from_env(Some("/tmp/fake-cargo-home")),
            Some(std::path::PathBuf::from("/tmp/fake-cargo-home").join("bin"))
        );
    }

    /// Why: An empty or absent `CARGO_HOME` must fall back to `~/.cargo/bin`.
    /// What: Both `Some("")` and `None` resolve to a path ending in `.cargo/bin`.
    /// Test: This is the test.
    #[test]
    fn cargo_bin_dir_from_env_falls_back() {
        for value in [Some(""), None] {
            if let Some(p) = cargo_bin_dir_from_env(value) {
                assert!(p.ends_with(std::path::Path::new(".cargo").join("bin")));
            }
        }
    }

    /// Why: An unknown member must be a clean error (exit 3), not a silent skip.
    /// What: Calls `run` with a bogus member in `--json` mode; asserts exit 3.
    /// Test: This is the test.
    #[test]
    fn run_unknown_member_is_error() {
        let code = run(&["not-a-real-tool".to_owned()], true, true, false);
        assert_eq!(code, 3);
    }
}
