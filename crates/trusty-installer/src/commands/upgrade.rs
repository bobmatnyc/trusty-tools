//! `tctl upgrade` — confirm-then-apply upgrades, then restart daemons (#1316).
//!
//! Why: The mutating half of the update flow. The #1316 added requirement makes
//! the safety property explicit: present the available updates and apply them
//! ONLY after confirmation (`--yes` skips the prompt for automation). Daemons
//! are restarted after the new binary lands, so launchd-supervised members come
//! back on it rather than continuing to serve the old one.
//!
//! What: Gathers candidates (`update_engine`), runs the pure confirm-then-apply
//! gate (`decide_apply`), and — only on `Apply` — upgrades each member: prebuilt
//! download first, `cargo install` as the fallback, then a concrete-path health
//! gate, then (daemons only) a restart through the same launchd path `tctl
//! restart` uses. Progress rows render via `trusty-progress`. `--check` is a
//! read-only alias for `tctl updates`.
//!
//! #4964: the daemon branches used to call
//! `trusty_common::update::upgrade_and_restart`, which ran `cargo install`
//! even when the prebuilt had just been placed (writing the binary to two
//! directories in one command) and whose restart step could not restart
//! anything when the caller is `tctl`. See `upgrade_one`'s doc.
//!
//! Test: `tests` covers the decision routing and the JSON report shaping; the
//! actual `cargo install` + restart path is side-effecting.

use serde::Serialize;
use tokio::runtime::Handle;
use trusty_progress::{Component, ComponentTracker};

use super::progress_ui::{is_tty, narrator, prompt_yes_no};
use super::runtime::with_runtime;
use super::shadow_check;
use super::stable_set::select_members;
use super::update_engine::{
    decide_apply, gather_candidates, ApplyDecision, ApplyInputs, UpdateCandidate,
};
use crate::output::render_json;

/// One member's upgrade outcome for the report.
///
/// Why: Typed per-member result keeps machine output stable + testable.
/// What: `member` crate name; `ok` success; `detail` human note / error / hint.
/// `shadow_ok` / `shadow_detail` (#3554) mirror `install::InstallOutcome`'s
/// fields of the same name: `shadow_ok` is `false` ONLY when the
/// just-upgraded binary is provably shadowed on `$PATH` by a different,
/// stale copy (checked for the non-daemon branches of `upgrade_one`; the
/// daemon-restart path is deliberately out of scope — see `upgrade_one`'s
/// doc) — never a silent success.
/// Test: `tests::report_serialises`, `tests::applied_report_all_ok`,
/// `tests::applied_report_all_ok_reflects_shadow_failure`.
#[derive(Clone, Debug, Serialize)]
pub struct UpgradeOutcome {
    /// Crate name upgraded.
    pub member: String,
    /// Whether the upgrade (+ restart for daemons) succeeded.
    pub ok: bool,
    /// Human detail: applied version, restart hint, or error.
    pub detail: String,
    /// `false` ONLY when the just-upgraded binary is shadowed on `$PATH` by
    /// a different, stale copy (#3554); `true` when clear, not checked
    /// (daemon path), or not applicable (a binary-upgrade failure).
    pub shadow_ok: bool,
    /// Human note for the shadow-detection outcome (empty when `shadow_ok`).
    pub shadow_detail: String,
}

/// The aggregate upgrade report.
///
/// Why: `--json` consumers want the whole rollup + a `status` discriminator that
/// distinguishes "applied", "declined", "needs-confirmation", and "nothing".
/// What: Holds the status string, the candidates considered, the per-member
/// outcomes, and `all_ok`.
/// Test: `tests::report_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct UpgradeReport {
    /// Fixed command tag.
    pub command: &'static str,
    /// `applied` | `declined` | `needs_confirmation` | `nothing_to_do`.
    pub status: &'static str,
    /// The candidates considered (always populated for transparency).
    pub candidates: Vec<UpdateCandidate>,
    /// Per-member outcomes (only populated when `status == "applied"`).
    pub members: Vec<UpgradeOutcome>,
    /// Whether every applied member succeeded (true when nothing was applied).
    pub all_ok: bool,
}

impl UpgradeReport {
    /// Build a non-applying report (declined / needs-confirmation / nothing).
    ///
    /// Why: The three non-mutating outcomes share a shape (no member outcomes).
    /// What: Sets `members = []`, `all_ok = true`.
    /// Test: `tests::report_serialises`.
    fn non_applying(status: &'static str, candidates: Vec<UpdateCandidate>) -> Self {
        Self {
            command: "upgrade",
            status,
            candidates,
            members: Vec::new(),
            all_ok: true,
        }
    }

    /// Build an applied report from per-member outcomes.
    ///
    /// Why: Centralises the `all_ok` derivation for the applied path — must
    /// not report success while a genuine PATH-shadow condition (#3554)
    /// leaves the just-upgraded binary unreachable from a plain shell
    /// invocation, mirroring `install::InstallReport::build`'s treatment of
    /// `shadow_ok`.
    /// What: Sets `status = "applied"`, `all_ok = every outcome ok AND
    /// shadow_ok`.
    /// Test: `tests::applied_report_all_ok`,
    /// `tests::applied_report_all_ok_reflects_shadow_failure`.
    fn applied(candidates: Vec<UpdateCandidate>, members: Vec<UpgradeOutcome>) -> Self {
        let all_ok = members.iter().all(|m| m.ok && m.shadow_ok);
        Self {
            command: "upgrade",
            status: "applied",
            candidates,
            members,
            all_ok,
        }
    }

    /// Process exit code per status.
    ///
    /// Why: Automation branches on this.
    /// What: `applied` → 0 if all_ok else 2; `needs_confirmation` → 3 (could not
    /// confirm — automation should pass `--yes`); `declined` → 4 (user said no);
    /// `nothing_to_do` → 0.
    /// Test: `tests::exit_codes_per_status`.
    fn exit_code(&self) -> i32 {
        match self.status {
            "applied" => {
                if self.all_ok {
                    0
                } else {
                    2
                }
            }
            "needs_confirmation" => 3,
            "declined" => 4,
            _ => 0,
        }
    }
}

/// Handle `tctl upgrade [<members>…] [--check] [--latest] [--exclude-self]`.
///
/// Why: Phase-1 confirm-then-apply upgrade (#1316 priority 2 + 3).
///
/// What: With `--check`, delegates to the read-only `updates` listing. Otherwise
/// gathers candidates, presents them, runs the pure confirm-then-apply gate, and
/// applies only on `Apply`. `exclude_self` drops `trusty-installer`/`tctl` from
/// the set (avoids upgrading the running binary mid-flight). Returns the exit code.
///
/// Test: `tests::run_check_is_readonly` (delegation); the apply path is
/// side-effecting and validated manually.
pub fn run(
    check: bool,
    latest: bool,
    exclude_self: bool,
    yes: bool,
    members: &[String],
    json: bool,
) -> i32 {
    // --check is a read-only alias for `tctl updates`.
    if check {
        return super::updates::run(latest, json);
    }

    let (mut selected, unknown) = select_members(members);
    if !unknown.is_empty() {
        let msg = format!("unknown member(s): {}", unknown.join(", "));
        if json {
            if render_json(&serde_json::json!({"command":"upgrade","error":msg})).is_err() {
                eprintln!("tctl upgrade: failed to write JSON output");
                return 1;
            }
        } else {
            eprintln!("tctl upgrade: {msg}");
        }
        return 3;
    }
    if exclude_self {
        // Exclude both the primary name and the transitional alias binary name
        // so --exclude-self works regardless of which binary the user invoked.
        // Also exclude "tctl" by binary name as belt-and-suspenders: trusty-installer
        // is not in stable_set(), so this guard would only fire if the alias were
        // ever added to the set — but we want to be explicit about the intent.
        selected.retain(|m| {
            m.crate_name != "trusty-installer"
                && m.binary != "trusty-installer"
                && m.binary != "tctl"
        });
    }

    // Build ONE runtime for the whole command: candidate gathering and applying
    // share the same handle (no per-future runtime construction, no nested
    // runtime panic risk).
    let report = with_runtime(|handle| {
        let candidates = handle.block_on(gather_candidates(&selected));
        resolve_and_apply(handle, candidates, yes, json)
    });
    if json {
        // A failed machine-readable write must not exit 0: automation would
        // read success from the exit code while the JSON never arrived.
        if render_json(&report).is_err() {
            eprintln!("tctl upgrade: failed to write JSON output");
            return 1;
        }
    } else {
        print_human(&report);
    }
    report.exit_code()
}

/// Present candidates, run the confirm-then-apply gate, and apply on `Apply`.
///
/// Why: The single place the #1316 safety property is enforced at the command
/// layer — it threads `decide_apply` between "what's available" and "do it".
///
/// What: Lists candidates (human/stderr), computes the [`ApplyInputs`] from
/// `--yes` + the TTY + (when prompting) an interactive yes/no, and matches the
/// resulting [`ApplyDecision`] into an [`UpgradeReport`]. Only `Apply` mutates.
///
/// Test: `tests::run_check_is_readonly`; the apply branch is side-effecting.
fn resolve_and_apply(
    handle: &Handle,
    candidates: Vec<UpdateCandidate>,
    yes: bool,
    json: bool,
) -> UpgradeReport {
    let has_candidates = !candidates.is_empty();

    if has_candidates && !json {
        let narr = narrator(json);
        let _ = narr.info(&format!("{} update(s) available:", candidates.len()));
        for c in &candidates {
            let installed = c.installed.as_deref().unwrap_or("(absent)");
            let _ = narr.info(&format!("  {} {} → {}", c.crate_name, installed, c.latest));
        }
    }

    // Probe the TTY exactly once and reuse the single binding for both the
    // prompt gate and `decide_apply`. If these ever disagreed (two separate
    // `is_tty()` calls), the user could answer "yes" yet `decide_apply` see
    // `is_tty=false` → NeedsConfirmation (silent refusal), or vice versa.
    let tty = is_tty();

    // Only prompt when we actually need to (candidates, no --yes, on a TTY).
    let interactive_consent = if should_prompt(has_candidates, yes, tty, json) {
        prompt_yes_no("Apply these upgrades now?")
    } else {
        false
    };

    let decision = decide_apply(build_apply_inputs(
        has_candidates,
        yes,
        tty,
        interactive_consent,
    ));

    match decision {
        ApplyDecision::NothingToDo => UpgradeReport::non_applying("nothing_to_do", candidates),
        ApplyDecision::NeedsConfirmation => {
            UpgradeReport::non_applying("needs_confirmation", candidates)
        }
        ApplyDecision::Decline => UpgradeReport::non_applying("declined", candidates),
        ApplyDecision::Apply => {
            let outcomes = handle.block_on(apply_all(&candidates, json));
            UpgradeReport::applied(candidates, outcomes)
        }
    }
}

/// Decide whether to show the interactive confirmation prompt.
///
/// Why: Extracted as a pure predicate so the prompt gate and the
/// [`ApplyInputs`] fed to `decide_apply` are guaranteed to read the *same*
/// `tty` value (see `resolve_and_apply`). A divergence here would let the user
/// consent on a prompt that `decide_apply` then treats as non-interactive.
/// What: Prompt only when there are candidates, `--yes` was not passed, we are
/// on a TTY, and we are not in `--json` mode.
/// Test: `tests::prompt_gate_matches_apply_inputs`.
fn should_prompt(has_candidates: bool, yes: bool, tty: bool, json: bool) -> bool {
    has_candidates && !yes && tty && !json
}

/// Build the [`ApplyInputs`] for the gate from a single `tty` probe.
///
/// Why: Centralising construction guarantees the `is_tty` field carries the
/// exact `tty` binding used by `should_prompt` — the consent and TTY values fed
/// to `decide_apply` can never disagree.
/// What: Threads the four decision inputs into an [`ApplyInputs`].
/// Test: `tests::prompt_gate_matches_apply_inputs`.
fn build_apply_inputs(
    has_candidates: bool,
    yes: bool,
    tty: bool,
    interactive_consent: bool,
) -> ApplyInputs {
    ApplyInputs {
        has_candidates,
        yes,
        is_tty: tty,
        interactive_consent,
    }
}

/// One candidate's health-gate result, carrying the shadow-check outcome
/// alongside the human detail string (#3554).
///
/// Why: `upgrade_one` needs to convey more than "succeeded with this detail"
/// for the non-daemon branches — a genuine PATH shadow must reach
/// `apply_all`'s `UpgradeOutcome` without collapsing into the plain success
/// path. A small typed return keeps that distinction explicit rather than
/// smuggling it through the detail string.
/// What: `detail` is the human-facing note; `shadow_ok`/`shadow_detail`
/// mirror `UpgradeOutcome`'s fields of the same name.
struct UpgradeDetail {
    detail: String,
    shadow_ok: bool,
    shadow_detail: String,
}

impl UpgradeDetail {
    /// A clean success with no shadow check performed or nothing found
    /// (the daemon-restart path; see `upgrade_one`'s doc for why that path
    /// does not run `shadow_check`).
    fn ok(detail: String) -> Self {
        Self {
            detail,
            shadow_ok: true,
            shadow_detail: String::new(),
        }
    }

    /// Fold a `shadow_check::detect` result into the detail: `None` (clear)
    /// stays a plain success; `Some(report)` flips `shadow_ok` to `false`
    /// and carries the actionable message.
    fn from_shadow(detail: String, shadow: Option<shadow_check::ShadowReport>) -> Self {
        match shadow {
            Some(report) => Self {
                detail,
                shadow_ok: false,
                shadow_detail: report.message(),
            },
            None => Self::ok(detail),
        }
    }
}

/// Apply every candidate upgrade, prebuilt-first with cargo fallback, restarting daemons.
///
/// Why: Prebuilt binaries upgrade in seconds without requiring a Rust toolchain;
/// the cargo path is the universal fallback for unsupported platforms and failures.
/// Daemons must restart via the connection-safe path after upgrade.
///
/// What: For each candidate, attempts a prebuilt download (Phase 2 / #1760),
/// falling back to `perform_upgrade` (`cargo install`), then health-gates the
/// concrete path. Non-daemons additionally run a PATH-shadow check (#3554);
/// daemons are restarted via `restart_daemon_member` (#4964).
///
/// Renders a per-member narration line + a final component table. A genuine
/// shadow is surfaced via `narr.error` (not `info`), same as a real failure,
/// even though the member's binary-upgrade `ok` stays `true`.
///
/// Test: Side-effecting; the prebuilt routing and report shaping are tested via
/// `UpgradeReport` and `crate::download::tests`.
async fn apply_all(candidates: &[UpdateCandidate], json: bool) -> Vec<UpgradeOutcome> {
    let narr = narrator(json);
    let mut tracker = ComponentTracker::new(narr.output());
    let mut outcomes = Vec::with_capacity(candidates.len());
    // #3554: the real $PATH, resolved once, used by every non-daemon
    // candidate's shadow-detection check below (mirrors `install::install_all`).
    let path_env = std::env::var_os("PATH").unwrap_or_default();

    for c in candidates {
        let _ = narr.info(&format!("upgrading {} → {}", c.crate_name, c.latest));

        let result = upgrade_one(c, &path_env).await;

        match result {
            Ok(d) => {
                if !d.shadow_ok {
                    let _ = narr.error(&format!("{}: {}", c.crate_name, d.shadow_detail));
                }
                outcomes.push(UpgradeOutcome {
                    member: c.crate_name.clone(),
                    ok: true,
                    detail: d.detail,
                    shadow_ok: d.shadow_ok,
                    shadow_detail: d.shadow_detail,
                });
                tracker.add(Component::new(c.binary.clone(), 0));
            }
            Err(e) => {
                let _ = narr.error(&format!("{}: {e}", c.crate_name));
                outcomes.push(UpgradeOutcome {
                    member: c.crate_name.clone(),
                    ok: false,
                    detail: e.to_string(),
                    shadow_ok: true,
                    shadow_detail: String::new(),
                });
            }
        }
    }
    if !json {
        let _ = tracker.print();
    }
    outcomes
}

/// Upgrade a single candidate, prebuilt-first with cargo/restart fallback.
///
/// Why: Extracted from `apply_all` to keep the loop body readable and to allow
/// the `?` operator to propagate errors cleanly per-candidate.
///
/// What: Tries `try_install_prebuilt`; on success the binary is on disk. On
/// fallback, runs `perform_upgrade` (`cargo install`). EITHER way the binary is
/// then health-gated at its CONCRETE path and, for a daemon member, activated
/// via [`restart_daemon_member`]. Non-daemon branches ALSO run
/// `shadow_check::detect` after the health gate passes (#3554 review — this was
/// missing from the initial fix: the health-gate half landed for `tctl upgrade`
/// but not the shadow-detection half, so a host with `~/.cargo/bin/tm`
/// shadowing `~/.local/bin/tm` got the right version logged with zero warning
/// that the shell still resolves the stale binary). `path_env` is the real
/// `$PATH`, threaded in from `apply_all` so it is resolved once per `apply_all`
/// call, not once per candidate.
///
/// 🔴 #4964 Phase 1 — this function no longer calls
/// `trusty_common::update::upgrade_and_restart`, on either daemon branch. Two
/// separate defects lived in that one call:
///
/// - **It double-installed.** On the prebuilt branch the binary was already on
///   disk in `install_dir`, and `upgrade_and_restart` then ran `cargo install
///   <crate> --locked` anyway, landing a second copy in `$CARGO_HOME/bin`. The
///   comment claiming that step was "a no-op if the binary is already current"
///   was wrong: `cargo install` skips only when Cargo's own `.crates2.json`
///   records that exact version, and the prebuilt path writes no cargo
///   metadata. Six of the seven stable-set members are daemons, so this fired
///   on nearly every upgrade — and on a machine with no Rust toolchain it
///   errored out AFTER the new binary had already landed.
/// - **It never restarted anything.** `upgrade_and_restart` restarts by calling
///   `std::process::exit(1)` and relying on launchd's `KeepAlive` to respawn
///   THE PROCESS THAT JUST EXITED. That is correct for its other callers
///   (`trusty-search upgrade`, `trusty-memory upgrade`, the two MCP `upgrade`
///   tools), which all run inside the supervised daemon. `tctl` is a terminal
///   process launchd has never heard of, so the supervision check returned
///   false every time and the function returned a manual-restart hint, which
///   `apply_all` reported as success. `tctl upgrade` has never restarted a
///   daemon member.
///
/// A "restart-only" variant of `upgrade_and_restart` would have fixed the first
/// defect and not the second, since `exit(1)` from `tctl` still restarts
/// nothing. The restart now goes through the same launchd path `tctl restart`
/// uses. `upgrade_and_restart` itself is unchanged and still serves its
/// in-daemon callers.
///
/// Test: Side-effecting; covered indirectly. `UpgradeDetail`'s shadow-folding
/// is covered by `tests::applied_report_all_ok_reflects_shadow_failure`, and
/// the restart routing by `tests::daemon_restart_routes_by_manage_strategy`.
async fn upgrade_one(
    c: &UpdateCandidate,
    path_env: &std::ffi::OsStr,
) -> anyhow::Result<UpgradeDetail> {
    use crate::download::{self, Outcome};

    // #4964: fall back to the SHARED `canonical_bin_dir()` rather than an
    // inline `CARGO_HOME` read (one of five copies of the same rule).
    let install_dir = download::default_install_dir()
        .or_else(trusty_common::bin_resolve::canonical_bin_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("/usr/local/bin"));

    let outcome = download::try_install_prebuilt(&c.crate_name, &install_dir).await;

    match outcome {
        Outcome::Installed { paths, version } => {
            tracing::info!(crate_name = %c.crate_name, %version, "upgraded from prebuilt");
            // #3554: health-gate the CONCRETE just-placed binary — the
            // exact path `download::try_install_prebuilt` reports having
            // written — never a name re-resolved afterward (mirrors
            // `install::install_one`'s fix for the same bug class). #4964
            // extends this to the daemon branch, which used to health-gate by
            // NAME inside `upgrade_and_restart`.
            let bin_path = paths
                .iter()
                .find(|p| p.file_name().and_then(|f| f.to_str()) == Some(c.binary.as_str()))
                .cloned()
                .unwrap_or_else(|| install_dir.join(&c.binary));
            trusty_common::update::verify_installed_binary_at_path(&bin_path).await?;
            if c.daemon {
                // #4964: no `cargo install` on this branch — the binary is
                // already on disk, health-gated, and only needs activating.
                let restarted = restart_daemon_member(c)?;
                Ok(UpgradeDetail::ok(format!(
                    "upgraded to {version}; {restarted}"
                )))
            } else {
                let shadow =
                    shadow_check::detect(&c.binary, &bin_path, Some(&version), path_env).await;
                Ok(UpgradeDetail::from_shadow(
                    format!("upgraded to {version}"),
                    shadow,
                ))
            }
        }
        Outcome::Fallback { reason } => {
            tracing::info!(crate_name = %c.crate_name, %reason, "prebuilt unavailable; using cargo install");
            // Verify cargo is available before attempting the fallback.
            which::which("cargo").map_err(|_| {
                anyhow::anyhow!(
                    "no Rust toolchain found on PATH (cargo not available); \
                     cannot fall back to `cargo install {}`",
                    c.crate_name
                )
            })?;
            trusty_common::update::perform_upgrade(&c.crate_name).await?;
            // #3554: `cargo install` always lands in the cargo bin dir —
            // resolve that CONCRETE destination directly rather than a name
            // lookup.
            let bin_path = trusty_common::bin_resolve::canonical_bin_dir()
                .unwrap_or_else(|| install_dir.clone())
                .join(&c.binary);
            let reported =
                trusty_common::update::verify_installed_binary_at_path(&bin_path).await?;
            if c.daemon {
                // #4964: the restart is NEW behaviour on this path too — it
                // previously produced a manual-restart hint reported as
                // success.
                let restarted = restart_daemon_member(c)?;
                Ok(UpgradeDetail::ok(format!(
                    "upgraded to {}; {restarted}",
                    c.latest
                )))
            } else {
                let reported_version = super::update_engine::extract_version_from_line(&reported);
                let shadow = shadow_check::detect(
                    &c.binary,
                    &bin_path,
                    reported_version.as_deref(),
                    path_env,
                )
                .await;
                Ok(UpgradeDetail::from_shadow(
                    format!("upgraded to {}", c.latest),
                    shadow,
                ))
            }
        }
    }
}

/// Activate a just-placed daemon binary by restarting the member (#4964).
///
/// Why: placing a binary on disk does nothing to a running daemon — launchd
/// keeps the old process, and its plist keeps pointing wherever it pointed
/// before. Until this landed, `tctl upgrade` produced exactly that state and
/// reported success. Routing through [`super::lifecycle::restart_member`] means
/// upgrade and `tctl restart` share ONE restart implementation, so the
/// port-guard-then-`bootout`-then-`bootstrap` ordering (#4470) and the
/// `ExitTimeOut` drain window apply here for free. `launchctl kickstart -k` is
/// deliberately not used: it sends `SIGKILL` and drops in-flight requests.
///
/// What: dispatches on [`super::stable_set::manage_strategy_for`] — the same
/// rule `tctl start|stop|restart` uses — so a launchd member is booted out and
/// back in, and trusty-mpm (process-managed) goes through its own `restart`
/// subcommand. On a non-macOS host a launchd member has no supervisor to
/// restart, so the binary-upgrade is reported with a manual-restart note rather
/// than a spurious failure.
///
/// Test: `tests::restart_plan_*` pin the dispatch exhaustively; the
/// `launchctl`/subprocess calls themselves are side-effecting and are never
/// invoked from a unit test (doing so would bounce the host's live daemons).
fn restart_daemon_member(c: &UpdateCandidate) -> anyhow::Result<String> {
    match restart_plan(&c.binary, c.daemon, cfg!(target_os = "macos")) {
        RestartPlan::NoRestart(note) => Ok(note),
        RestartPlan::Restart(strategy) => super::lifecycle::restart_member(&c.binary, strategy),
    }
}

/// What restarting a just-upgraded member requires, decided without doing it.
///
/// Why: the side effect here bounces a live daemon, so the DECISION has to be
/// separable from the act or it cannot be tested at all — and "does an upgraded
/// daemon get restarted?" is the whole of #4964's Phase 1a. Pinning it as data
/// means a future edit that quietly drops the restart for some member class
/// fails a test instead of silently reproducing the defect.
/// What: [`RestartPlan::Restart`] carries the lifecycle strategy to apply;
/// [`RestartPlan::NoRestart`] carries the reason nothing is bounced.
/// Test: `tests::restart_plan_*`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RestartPlan {
    /// Bounce the member using this strategy.
    Restart(super::stable_set::ManageStrategy),
    /// Nothing to bounce; the payload is the human note for the report.
    NoRestart(String),
}

/// Decide how a just-upgraded member should be restarted.
///
/// Why: see [`RestartPlan`]. `macos` is a parameter rather than a `cfg!` read
/// so both platform answers are testable from either host.
/// What: a non-daemon needs no restart. trusty-mpm is process-managed
/// ([`ManageStrategy::OwnVerb`]) and restarts on any platform. Every other
/// daemon is launchd-managed, which exists only on macOS — elsewhere the binary
/// is upgraded and the operator is told to restart it, rather than the upgrade
/// being reported as failed for a supervisor the host does not have.
/// Test: `tests::restart_plan_daemons_restart`,
/// `tests::restart_plan_non_daemon_is_a_noop`,
/// `tests::restart_plan_launchd_member_off_macos_is_manual`.
fn restart_plan(binary: &str, daemon: bool, macos: bool) -> RestartPlan {
    use super::stable_set::{manage_strategy_for, ManageStrategy};

    match manage_strategy_for(binary, daemon) {
        ManageStrategy::None => {
            RestartPlan::NoRestart("no restart needed (not a daemon)".to_owned())
        }
        ManageStrategy::Launchd if !macos => RestartPlan::NoRestart(format!(
            "restart the {binary} daemon manually (launchd is macOS-only)"
        )),
        strategy => RestartPlan::Restart(strategy),
    }
}

/// Render the human-readable upgrade summary footer.
///
/// Why: Operators want a clear verdict after the confirm-then-apply flow.
/// What: Branches on the report status to print the right one-liner.
/// Test: Side-effect-only; the data is tested via `UpgradeReport`.
fn print_human(report: &UpgradeReport) {
    match report.status {
        "nothing_to_do" => println!("All stable-set tools are up to date."),
        "needs_confirmation" => eprintln!(
            "tctl upgrade: {} update(s) available but not applied — re-run on a terminal or pass --yes to confirm.",
            report.candidates.len()
        ),
        "declined" => eprintln!("tctl upgrade: aborted (no confirmation)."),
        "applied" => {
            let ok = report.members.iter().filter(|m| m.ok).count();
            println!("upgraded {}/{}", ok, report.members.len());
            for m in report.members.iter().filter(|m| !m.ok) {
                eprintln!("  failed: {} — {}", m.member, m.detail);
            }
            // #3554: a binary can upgrade cleanly (`ok: true`) while the
            // operator's shell still resolves a stale, PATH-shadowed copy —
            // surface that on the human path too, not just fold it silently
            // into the exit code.
            for m in report.members.iter().filter(|m| m.ok && !m.shadow_ok) {
                eprintln!("  {} — {}", m.member, m.shadow_detail);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
#[path = "upgrade_tests.rs"]
mod tests;
