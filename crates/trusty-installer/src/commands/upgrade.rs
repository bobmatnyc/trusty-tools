//! `tctl upgrade` — confirm-then-apply upgrades, then restart daemons (#1316).
//!
//! Why: The mutating half of the update flow. The #1316 added requirement makes
//! the safety property explicit: present the available updates and apply them
//! ONLY after confirmation (`--yes` skips the prompt for automation). Daemons
//! are restarted cleanly after upgrade via `upgrade_and_restart` so launchd-
//! supervised members come back on the new binary.
//!
//! What: Gathers candidates (`update_engine`), runs the pure confirm-then-apply
//! gate (`decide_apply`), and — only on `Apply` — upgrades each member. Daemons
//! go through `upgrade_and_restart` (cargo install + health-gate + connection-
//! safe restart); non-daemons go through `perform_upgrade` + `verify_installed_
//! binary`. Progress rows render via `trusty-progress`. `--check` is a read-only
//! alias for `tctl updates`.
//!
//! Test: `tests` covers the decision routing and the JSON report shaping; the
//! actual `cargo install` + restart path is side-effecting.

use serde::Serialize;
use tokio::runtime::Handle;
use trusty_progress::{Component, ComponentTracker};

use super::progress_ui::{is_tty, narrator, prompt_yes_no};
use super::runtime::with_runtime;
use super::stable_set::select_members;
use super::update_engine::{
    decide_apply, gather_candidates, ApplyDecision, ApplyInputs, UpdateCandidate,
};
use crate::output::render_json;

/// One member's upgrade outcome for the report.
///
/// Why: Typed per-member result keeps machine output stable + testable.
/// What: `member` crate name; `ok` success; `detail` human note / error / hint.
/// Test: `tests::report_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct UpgradeOutcome {
    /// Crate name upgraded.
    pub member: String,
    /// Whether the upgrade (+ restart for daemons) succeeded.
    pub ok: bool,
    /// Human detail: applied version, restart hint, or error.
    pub detail: String,
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
    /// Why: Centralises the `all_ok` derivation for the applied path.
    /// What: Sets `status = "applied"`, `all_ok = every outcome ok`.
    /// Test: `tests::applied_report_all_ok`.
    fn applied(candidates: Vec<UpdateCandidate>, members: Vec<UpgradeOutcome>) -> Self {
        let all_ok = members.iter().all(|m| m.ok);
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

/// Apply every candidate upgrade, prebuilt-first with cargo fallback, restarting daemons.
///
/// Why: Prebuilt binaries upgrade in seconds without requiring a Rust toolchain;
/// the cargo path is the universal fallback for unsupported platforms and failures.
/// Daemons must restart via the connection-safe path after upgrade.
///
/// What: For each candidate, attempts a prebuilt download (Phase 2 / #1760):
/// - Non-daemons: `try_install_prebuilt` → fallback to `perform_upgrade` +
///   `verify_installed_binary`.
/// - Daemons: `try_install_prebuilt` (updates the binary on disk) → fallback to
///   `upgrade_and_restart` (cargo install + connection-safe restart).
///
/// Renders a per-member narration line + a final component table.
///
/// Test: Side-effecting; the prebuilt routing and report shaping are tested via
/// `UpgradeReport` and `crate::download::tests`.
async fn apply_all(candidates: &[UpdateCandidate], json: bool) -> Vec<UpgradeOutcome> {
    let narr = narrator(json);
    let mut tracker = ComponentTracker::new(narr.output());
    let mut outcomes = Vec::with_capacity(candidates.len());

    for c in candidates {
        let _ = narr.info(&format!("upgrading {} → {}", c.crate_name, c.latest));

        let result = upgrade_one(c).await;

        match result {
            Ok(detail) => {
                outcomes.push(UpgradeOutcome {
                    member: c.crate_name.clone(),
                    ok: true,
                    detail,
                });
                tracker.add(Component::new(c.binary.clone(), 0));
            }
            Err(e) => {
                let _ = narr.error(&format!("{}: {e}", c.crate_name));
                outcomes.push(UpgradeOutcome {
                    member: c.crate_name.clone(),
                    ok: false,
                    detail: e.to_string(),
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
/// What: Tries `try_install_prebuilt`; on success the binary is on disk. For
/// daemons, always runs the restart step (via `upgrade_and_restart` or a
/// post-placement restart) to activate the new binary. On fallback, runs
/// `upgrade_and_restart` (daemons) or `perform_upgrade` +
/// `verify_installed_binary` (non-daemons). Returns a human detail string.
///
/// Test: Side-effecting; covered indirectly.
async fn upgrade_one(c: &UpdateCandidate) -> anyhow::Result<String> {
    use crate::download::{self, Outcome};

    let install_dir = download::default_install_dir().unwrap_or_else(|| {
        let cargo_home = std::env::var("CARGO_HOME").unwrap_or_default();
        if cargo_home.is_empty() {
            dirs::home_dir()
                .map(|h| h.join(".cargo").join("bin"))
                .unwrap_or_else(|| std::path::PathBuf::from("/usr/local/bin"))
        } else {
            std::path::PathBuf::from(&cargo_home).join("bin")
        }
    });

    let outcome = download::try_install_prebuilt(&c.crate_name, &install_dir).await;

    match outcome {
        Outcome::Installed { version, .. } => {
            tracing::info!(crate_name = %c.crate_name, %version, "upgraded from prebuilt");
            if c.daemon {
                // Binary is now on disk at install_dir; trigger a daemon restart.
                // We do this via upgrade_and_restart so the connection-safe
                // restart protocol (SIGTERM + drain + launchd KeepAlive) is followed.
                // The `perform_upgrade` step inside upgrade_and_restart is a no-op
                // if the binary is already current, so this is safe.
                let hint = trusty_common::update::upgrade_and_restart(&c.crate_name, &c.binary)
                    .await
                    .map(|h| h.unwrap_or_else(|| "restarted".to_owned()))?;
                Ok(hint)
            } else {
                trusty_common::update::verify_installed_binary(&c.binary).await?;
                Ok(format!("upgraded to {version}"))
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
            if c.daemon {
                trusty_common::update::upgrade_and_restart(&c.crate_name, &c.binary)
                    .await
                    .map(|hint| hint.unwrap_or_else(|| "restarted".to_owned()))
            } else {
                match trusty_common::update::perform_upgrade(&c.crate_name).await {
                    Ok(()) => trusty_common::update::verify_installed_binary(&c.binary)
                        .await
                        .map(|()| format!("upgraded to {}", c.latest)),
                    Err(e) => Err(e),
                }
            }
        }
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
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate() -> UpdateCandidate {
        UpdateCandidate {
            crate_name: "trusty-search".to_owned(),
            binary: "trusty-search".to_owned(),
            installed: Some("0.19.0".to_owned()),
            latest: "0.20.0".to_owned(),
            daemon: true,
            is_install: false,
        }
    }

    /// Why: The non-applying envelope is a public contract; pin its shape.
    /// What: Builds a needs-confirmation report; asserts keys.
    /// Test: This is the test.
    #[test]
    fn report_serialises() {
        let report = UpgradeReport::non_applying("needs_confirmation", vec![candidate()]);
        let v = serde_json::to_value(&report).expect("serialises");
        assert_eq!(v["command"], "upgrade");
        assert_eq!(v["status"], "needs_confirmation");
        assert_eq!(v["candidates"][0]["crate_name"], "trusty-search");
        assert_eq!(v["members"].as_array().expect("array").len(), 0);
    }

    /// Why: An applied report's `all_ok` must reflect member outcomes.
    /// What: Mixes ok + failed; asserts all_ok false and status "applied".
    /// Test: This is the test.
    #[test]
    fn applied_report_all_ok() {
        let report = UpgradeReport::applied(
            vec![candidate()],
            vec![
                UpgradeOutcome {
                    member: "a".to_owned(),
                    ok: true,
                    detail: String::new(),
                },
                UpgradeOutcome {
                    member: "b".to_owned(),
                    ok: false,
                    detail: "boom".to_owned(),
                },
            ],
        );
        assert!(!report.all_ok);
        assert_eq!(report.status, "applied");
    }

    /// Why: Exit codes are the automation contract; pin each status mapping.
    /// What: Asserts the four status → exit-code mappings.
    /// Test: This is the test.
    #[test]
    fn exit_codes_per_status() {
        assert_eq!(
            UpgradeReport::non_applying("nothing_to_do", vec![]).exit_code(),
            0
        );
        assert_eq!(
            UpgradeReport::non_applying("needs_confirmation", vec![candidate()]).exit_code(),
            3
        );
        assert_eq!(
            UpgradeReport::non_applying("declined", vec![candidate()]).exit_code(),
            4
        );
        let applied_ok = UpgradeReport::applied(
            vec![candidate()],
            vec![UpgradeOutcome {
                member: "a".to_owned(),
                ok: true,
                detail: String::new(),
            }],
        );
        assert_eq!(applied_ok.exit_code(), 0);
    }

    /// Why: The load-bearing safety invariant from re-review: the `tty` value
    /// that gates the interactive prompt MUST be the same value fed to
    /// `decide_apply` as `is_tty`. A divergence would let a user answer "yes"
    /// while the gate silently treats the session as non-interactive (or the
    /// reverse). This pins the single-source-of-truth `tty` binding.
    /// What: For every (has_candidates, yes, tty, json) combination, asserts
    /// (a) `build_apply_inputs.is_tty` equals the probed `tty`, and (b) whenever
    /// `should_prompt` is true the same `tty` was true — so the consent path and
    /// the gate's TTY view are always consistent.
    /// Test: This is the test.
    #[test]
    fn prompt_gate_matches_apply_inputs() {
        for &has_candidates in &[true, false] {
            for &yes in &[true, false] {
                for &tty in &[true, false] {
                    for &json in &[true, false] {
                        let prompt = should_prompt(has_candidates, yes, tty, json);
                        // The consent the prompt would yield is irrelevant to
                        // the invariant; what matters is the TTY value is shared.
                        let inputs = build_apply_inputs(has_candidates, yes, tty, prompt);
                        // (a) the gate sees exactly the probed tty.
                        assert_eq!(inputs.is_tty, tty);
                        // (b) we only ever prompt when that same tty is true.
                        if prompt {
                            assert!(tty, "prompted without a TTY the gate also sees");
                        }
                    }
                }
            }
        }
    }

    /// Why: An unknown member must fail fast at selection (exit 3) and never
    /// reach the network/apply path — a pure, offline-safe guard test.
    /// What: Calls `run` with a bogus member in `--json` mode; asserts exit 3.
    /// Test: This is the test.
    #[test]
    fn run_unknown_member_is_error() {
        let code = run(false, false, false, true, &["not-a-tool".to_owned()], true);
        assert_eq!(code, 3);
    }

    /// Why: `--check` must be read-only (delegates to `updates`) and never reach
    /// the apply path. This performs a live crates.io probe, so it is
    /// `#[ignore]`-tagged to keep CI fast and offline-deterministic.
    /// What: Calls `run` with `check = true`; asserts exit 0 (read-only).
    /// Test: `cargo test -p trusty-installer -- --include-ignored`.
    #[test]
    #[ignore = "performs a live crates.io probe; run with --include-ignored"]
    fn run_check_is_readonly() {
        let code = run(true, false, false, false, &["tga".to_owned()], true);
        assert_eq!(code, 0);
    }
}
