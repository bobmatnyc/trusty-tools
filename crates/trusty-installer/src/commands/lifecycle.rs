//! `tctl start`, `tctl stop`, `tctl restart` — daemon lifecycle commands.
//!
//! Why: Lifecycle commands bring daemons up, take them down, or bounce them
//! connection-safely (SIGTERM drain, #534). They are system-scope and require a
//! blast-radius confirmation unless `--yes` is set. Crucially, not every member
//! is launchd-managed: the shared daemons (search/memory/analyze/review/console)
//! register launchd agents and are controlled via `launchctl bootstrap`/`bootout`,
//! while trusty-mpm is process-managed and controlled via its own
//! `trusty-mpm start|stop|restart` subcommands (#1332 decision 3). Encoding the
//! per-member [`ManageStrategy`](super::stable_set::ManageStrategy) lets one
//! handler drive both correctly.
//!
//! What: resolves members via `stable_set::select_members`, filters to daemons,
//! confirms the blast radius (mirroring `install::run`), then applies the verb in
//! topological order (start = forward, stop = reverse, restart = bootout then
//! bootstrap per member). launchd members use the shared
//! `trusty_common::launchd::LaunchdConfig` `bootout`/`bootstrap` (macOS-only);
//! `OwnVerb` members shell out to their own subcommand.
//!
//! Test: `tests` covers ordering, the report shaping/exit code, the blast-radius
//! gate, and the `Action` verb-name mapping; the actual launchctl/subprocess
//! calls are side-effecting and gated behind helpers (never invoked in tests).

use serde::Serialize;

use super::stable_set::{select_members, ManageStrategy, StableMember};
use crate::output::render_json;

/// The lifecycle verb being applied.
///
/// Why: start/stop/restart share the same orchestration (resolve → confirm →
/// per-member apply) but differ in ordering and the underlying action; encoding
/// the verb as an enum keeps one code path with three small branch points.
/// What: `Start` (forward order, bring up), `Stop` (reverse order, take down),
/// `Restart` (bootout then bootstrap per member).
/// Test: `tests::ordering_is_directional`, `tests::verb_name`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verb {
    /// Bring daemons up (forward topological order).
    Start,
    /// Take daemons down (reverse topological order).
    Stop,
    /// Bounce daemons (stop then start each).
    Restart,
}

impl Verb {
    /// The lower-case command name for labels and member subcommands.
    ///
    /// Why: used both in the human/JSON report and as the `<binary> <verb>`
    /// subcommand for process-managed members; one source avoids drift.
    /// What: `"start"` / `"stop"` / `"restart"`.
    /// Test: `tests::verb_name`.
    fn name(self) -> &'static str {
        match self {
            Verb::Start => "start",
            Verb::Stop => "stop",
            Verb::Restart => "restart",
        }
    }
}

/// One member's lifecycle outcome for the report.
///
/// Why: a typed per-member result keeps the `--json` output stable + testable.
/// What: `member` crate name; `ok` whether the action succeeded; `detail` a
/// human note (action taken or the error).
/// Test: `tests::report_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct LifecycleOutcome {
    /// Crate name acted upon.
    pub member: String,
    /// Whether the lifecycle action succeeded.
    pub ok: bool,
    /// Human detail / error message.
    pub detail: String,
}

/// The aggregate lifecycle report.
///
/// Why: `--json` consumers want the rollup + verdict in one object.
/// What: holds the verb, per-member outcomes, and the computed `all_ok`.
/// Test: `tests::report_serialises`, `tests::exit_code_reflects_all_ok`.
#[derive(Clone, Debug, Serialize)]
pub struct LifecycleReport {
    /// The verb applied (`start`/`stop`/`restart`).
    pub verb: Verb,
    /// Per-member outcomes in application order.
    pub members: Vec<LifecycleOutcome>,
    /// Whether every member's action succeeded.
    pub all_ok: bool,
}

impl LifecycleReport {
    /// Build a report and derive `all_ok`.
    ///
    /// Why: one place derives the verdict so JSON + exit code agree.
    /// What: `all_ok = every outcome ok` (vacuously true on an empty set).
    /// Test: `tests::exit_code_reflects_all_ok`.
    fn build(verb: Verb, members: Vec<LifecycleOutcome>) -> Self {
        let all_ok = members.iter().all(|m| m.ok);
        Self {
            verb,
            members,
            all_ok,
        }
    }

    /// Process exit code: 0 all ok, 2 any failed.
    ///
    /// Why: automation branches on this.
    /// What: `0` if `all_ok`, else `2`.
    /// Test: `tests::exit_code_reflects_all_ok`.
    fn exit_code(&self) -> i32 {
        if self.all_ok {
            0
        } else {
            2
        }
    }
}

/// Order the daemon members for a verb (start forward, stop/restart reverse).
///
/// Why: dependencies must come up before dependents and go down after them;
/// `start` walks the topological (stable-set) order, `stop`/`restart` walk it in
/// reverse so dependents stop first.
///
/// Note on `restart`: restart is a PER-MEMBER ROLLING bounce — each member is
/// stopped-then-started in turn (`apply_to_member` does bootout+bootstrap for one
/// member before moving to the next). It is NOT a stop-all-then-start-all barrier.
/// The reversed iteration order here is purely about teardown grouping (so a
/// dependent is bounced before the dependency it relies on); it does not imply
/// the whole stack goes down before any member comes back up.
///
/// What: filters `selected` to daemons (non-`None` strategy) and reverses for
/// `Stop`/`Restart`.
/// Test: `tests::ordering_is_directional`.
fn order_for(verb: Verb, selected: &[StableMember]) -> Vec<StableMember> {
    let mut daemons: Vec<StableMember> = selected
        .iter()
        .filter(|m| m.manage != ManageStrategy::None)
        .cloned()
        .collect();
    if verb != Verb::Start {
        daemons.reverse();
    }
    daemons
}

/// Handle `tctl start [<members>…]`.
///
/// Why: Phase-2 entry point — bring the selected daemons up.
/// What: forwards to [`run_lifecycle`] with [`Verb::Start`].
/// Test: side-effecting; the pure pieces are tested via `order_for`/`LifecycleReport`.
pub fn run_start(members: &[String], yes: bool, json: bool) -> i32 {
    run_lifecycle(Verb::Start, members, yes, json)
}

/// Handle `tctl stop [<members>…]`.
///
/// Why: Phase-2 entry point — take the selected daemons down (graceful SIGTERM).
/// What: forwards to [`run_lifecycle`] with [`Verb::Stop`].
/// Test: side-effecting; see `run_start`.
pub fn run_stop(members: &[String], yes: bool, json: bool) -> i32 {
    run_lifecycle(Verb::Stop, members, yes, json)
}

/// Handle `tctl restart [<members>…]`.
///
/// Why: Phase-2 entry point — bounce the selected daemons connection-safely.
/// What: forwards to [`run_lifecycle`] with [`Verb::Restart`].
/// Test: side-effecting; see `run_start`.
pub fn run_restart(members: &[String], yes: bool, json: bool) -> i32 {
    run_lifecycle(Verb::Restart, members, yes, json)
}

/// Shared orchestration for all three lifecycle verbs.
///
/// Why: the three verbs share resolve → confirm → ordered-apply; one helper
/// avoids triplication (DRY).
/// What: resolves members (unknown → exit 3), orders them per verb, confirms the
/// blast radius unless `--yes`/non-interactive/`--json`, applies the verb to each
/// member via [`apply_to_member`], then renders the report and returns its exit
/// code.
/// Test: the pure ordering + report are tested; the apply step is side-effecting.
fn run_lifecycle(verb: Verb, members: &[String], yes: bool, json: bool) -> i32 {
    let (selected, unknown) = select_members(members);
    if !unknown.is_empty() {
        let msg = format!("unknown member(s): {}", unknown.join(", "));
        if json {
            let _ = render_json(&serde_json::json!({ "command": verb.name(), "error": msg }));
        } else {
            eprintln!("tctl {}: {msg}", verb.name());
        }
        return 3;
    }

    let ordered = order_for(verb, &selected);
    if ordered.is_empty() {
        eprintln!("tctl {}: no daemon members selected", verb.name());
        return 0;
    }

    if !yes && !json && super::progress_ui::is_tty() {
        let names: Vec<&str> = ordered.iter().map(|m| m.crate_name.as_str()).collect();
        let q = format!(
            "{} {} daemon(s): {}? ",
            verb.name(),
            ordered.len(),
            names.join(", ")
        );
        if !super::progress_ui::prompt_yes_no(&q) {
            eprintln!("tctl {}: aborted (no confirmation).", verb.name());
            return 3;
        }
    }

    let outcomes: Vec<LifecycleOutcome> = ordered
        .iter()
        .map(|m| match apply_to_member(verb, m) {
            Ok(detail) => LifecycleOutcome {
                member: m.crate_name.clone(),
                ok: true,
                detail,
            },
            Err(e) => {
                eprintln!("tctl {}: {}: {e:#}", verb.name(), m.crate_name);
                LifecycleOutcome {
                    member: m.crate_name.clone(),
                    ok: false,
                    detail: e.to_string(),
                }
            }
        })
        .collect();

    let report = LifecycleReport::build(verb, outcomes);
    if json {
        if render_json(&report).is_err() {
            eprintln!("tctl {}: failed to write JSON output", verb.name());
            return 1;
        }
    } else {
        for m in &report.members {
            let mark = if m.ok { "ok" } else { "FAILED" };
            println!("  {:<18} {:<8} {}", m.member, mark, m.detail);
        }
    }
    report.exit_code()
}

/// Apply a lifecycle verb to a single member, dispatching on its strategy.
///
/// Why: launchd and process-managed members need different mechanisms; this is
/// the one place that branches on [`ManageStrategy`].
/// What: `Launchd` → [`launchd_control`] (macOS `launchctl`); `OwnVerb` →
/// `<binary> <verb>` subprocess; `None` → a no-op note (filtered out earlier, so
/// unreachable in practice). Returns a human detail string on success.
/// Test: side-effecting; the verb-name + ordering logic it relies on is tested.
fn apply_to_member(verb: Verb, m: &StableMember) -> anyhow::Result<String> {
    match m.manage {
        ManageStrategy::Launchd => launchd_control(verb, &m.binary),
        ManageStrategy::OwnVerb => own_verb_control(verb, &m.binary),
        ManageStrategy::None => Ok("skipped (not a daemon)".to_owned()),
    }
}

/// Drive a launchd-managed member via `launchctl bootstrap`/`bootout`.
///
/// Why: the shared daemons register `~/Library/LaunchAgents/<label>.plist`;
/// controlling them means bootstrapping/bootouting that agent (graceful SIGTERM
/// on bootout, #534). The label is resolved via the verified override table in
/// [`super::plist_label`].
/// What (macOS): builds a minimal `LaunchdConfig` carrying only the resolved
/// `label` (the field `bootout`/`bootstrap` actually use) and calls `bootout`
/// (stop), `bootstrap` (start), or both (restart). For `Start` (#2556) the
/// "plist must already exist" assumption is relaxed: if no plist is present for
/// this tctl-managed member, it runs the daemon's own `<binary> service
/// install` (which writes AND bootstraps the plist) instead of failing. On
/// non-macOS this is unsupported (launchd is macOS-only) and returns an error.
/// Test: the `Start` plist-presence decision is pinned by
/// `super::service_bootstrap::tests::start_plan_maps_presence`; the actual
/// `launchctl`/subprocess calls are side-effecting and never run in unit tests.
#[cfg(target_os = "macos")]
fn launchd_control(verb: Verb, binary: &str) -> anyhow::Result<String> {
    use super::service_bootstrap::{start_plan, RealServiceEnv, ServiceEnv, StartPlan};
    use trusty_common::launchd::{KeepAlive, LaunchdConfig};

    // Only `label` matters for bootout/bootstrap; the rest are inert because we
    // never render or install a plist here (the daemon's own `service install`
    // did that — and, for Start on a fresh machine, we now trigger it below).
    let cfg = LaunchdConfig {
        label: super::plist_label::plist_label_for(binary),
        exe_path: std::path::PathBuf::from(binary),
        args: Vec::new(),
        log_dir: std::path::PathBuf::from("/tmp"),
        keep_alive: KeepAlive::Always,
        throttle_interval: 0,
        env_vars: Vec::new(),
        fd_limit: None,
    };
    match verb {
        Verb::Stop => {
            cfg.bootout()?;
            Ok(format!("booted out {}", cfg.label))
        }
        Verb::Start => {
            // #2556: relax the "plist must already exist" assumption. If the
            // plist is present, bootstrap it (existing behaviour); if absent,
            // run the daemon's own `service install` to write + bootstrap it.
            let present = super::plist_label::plist_path_for(binary)
                .map(|p| p.exists())
                .unwrap_or(false);
            match start_plan(present) {
                StartPlan::Bootstrap => {
                    // #2566 review (item 4): `bootstrap()` boots the agent OUT
                    // before reloading it — a redundant restart when `tctl
                    // start` runs right after `tctl install` already bootstrapped
                    // this same daemon seconds earlier. Skip the bounce when the
                    // agent is already loaded; this is a cheap `launchctl print`
                    // check (mirrors the daemons' own `service status`).
                    if cfg.is_loaded() {
                        return Ok(format!("{} already running (skipped restart)", cfg.label));
                    }
                    cfg.bootstrap()?;
                    Ok(format!("bootstrapped {}", cfg.label))
                }
                StartPlan::ServiceInstall => {
                    RealServiceEnv.run_service_install(binary).map_err(|e| {
                        anyhow::anyhow!(
                            "no launchd plist for {binary} and `service install` failed: {e}"
                        )
                    })?;
                    Ok(format!("installed + bootstrapped {}", cfg.label))
                }
            }
        }
        Verb::Restart => {
            // Bootout first (stop); only then bootstrap (start). If bootstrap
            // fails after a successful bootout, surface that the daemon WAS
            // stopped — the operator is now in a stopped (not still-running)
            // state, which changes their next action.
            cfg.bootout()?;
            cfg.bootstrap()
                .map_err(|e| anyhow::anyhow!("booted out successfully; bootstrap failed: {e}"))?;
            Ok(format!("restarted {}", cfg.label))
        }
    }
}

/// Non-macOS stub: launchd is unavailable, so launchd control is unsupported.
///
/// Why: the crate must compile on Linux/CI; launchd lifecycle is macOS-only.
/// What: always returns an error explaining the platform limitation.
/// Test: compile-only on non-macOS targets.
#[cfg(not(target_os = "macos"))]
fn launchd_control(_verb: Verb, binary: &str) -> anyhow::Result<String> {
    anyhow::bail!("launchd control of `{binary}` is only supported on macOS")
}

/// Drive a process-managed member via its own `<binary> <verb>` subcommand.
///
/// Why: trusty-mpm is NOT launchd-managed — it ships `trusty-mpm start|stop|
/// restart` that spawn/`pkill` the daemon. Shelling to those is the only correct
/// way to control its lifecycle (#1332 decision 3).
/// What: spawns `<binary> <verb>`; maps a non-zero exit into an `Err`.
/// Test: side-effecting subprocess; never exercised in unit tests.
fn own_verb_control(verb: Verb, binary: &str) -> anyhow::Result<String> {
    if which::which(binary).is_err() {
        anyhow::bail!("{binary} is not installed (not on PATH)");
    }
    let status = std::process::Command::new(binary)
        .arg(verb.name())
        .status()
        .map_err(|e| anyhow::anyhow!("failed to spawn `{binary} {}`: {e}", verb.name()))?;
    if status.success() {
        Ok(format!("{binary} {} ok", verb.name()))
    } else {
        anyhow::bail!("`{binary} {}` exited with {status}", verb.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(binary: &str, manage: ManageStrategy) -> StableMember {
        StableMember {
            crate_name: binary.to_owned(),
            binary: binary.to_owned(),
            daemon: manage != ManageStrategy::None,
            manage,
        }
    }

    /// Why: start must be forward order, stop/restart reverse, and non-daemons
    /// must be filtered out.
    /// What: orders a 3-member list for each verb; asserts direction + filtering.
    /// Test: This is the test.
    #[test]
    fn ordering_is_directional() {
        let sel = vec![
            member("trusty-search", ManageStrategy::Launchd),
            member("tga", ManageStrategy::None),
            member("trusty-mpm", ManageStrategy::OwnVerb),
        ];
        let start: Vec<String> = order_for(Verb::Start, &sel)
            .into_iter()
            .map(|m| m.binary)
            .collect();
        assert_eq!(start, vec!["trusty-search", "trusty-mpm"]);

        let stop: Vec<String> = order_for(Verb::Stop, &sel)
            .into_iter()
            .map(|m| m.binary)
            .collect();
        assert_eq!(stop, vec!["trusty-mpm", "trusty-search"]);

        let restart: Vec<String> = order_for(Verb::Restart, &sel)
            .into_iter()
            .map(|m| m.binary)
            .collect();
        assert_eq!(restart, vec!["trusty-mpm", "trusty-search"]);
    }

    /// Why: the verb name is used as both a label and a subcommand; pin it.
    /// What: asserts each verb's name.
    /// Test: This is the test.
    #[test]
    fn verb_name() {
        assert_eq!(Verb::Start.name(), "start");
        assert_eq!(Verb::Stop.name(), "stop");
        assert_eq!(Verb::Restart.name(), "restart");
    }

    /// Why: the JSON envelope is a public contract; pin its shape.
    /// What: builds a report and asserts keys.
    /// Test: This is the test.
    #[test]
    fn report_serialises() {
        let report = LifecycleReport::build(
            Verb::Start,
            vec![LifecycleOutcome {
                member: "trusty-search".to_owned(),
                ok: true,
                detail: "bootstrapped com.trusty.trusty-search".to_owned(),
            }],
        );
        let v = serde_json::to_value(&report).expect("serialises");
        assert_eq!(v["verb"], "start");
        assert_eq!(v["all_ok"], true);
        assert_eq!(v["members"][0]["member"], "trusty-search");
    }

    /// Why: the exit code must track the verdict for automation.
    /// What: asserts 0 for all-ok, 2 for any failure.
    /// Test: This is the test.
    #[test]
    fn exit_code_reflects_all_ok() {
        let ok = LifecycleReport::build(
            Verb::Stop,
            vec![LifecycleOutcome {
                member: "a".to_owned(),
                ok: true,
                detail: String::new(),
            }],
        );
        assert_eq!(ok.exit_code(), 0);
        let bad = LifecycleReport::build(
            Verb::Stop,
            vec![LifecycleOutcome {
                member: "a".to_owned(),
                ok: false,
                detail: "boom".to_owned(),
            }],
        );
        assert_eq!(bad.exit_code(), 2);
    }

    /// Why: a process-managed member (mpm) must route through its own verb, not
    /// launchd. `apply_to_member` for an absent OwnVerb binary must error with a
    /// "not installed" message (proving it took the OwnVerb path, not launchd).
    /// What: applies Start to an absent OwnVerb member; asserts the error text.
    /// Test: This is the test.
    #[test]
    fn own_verb_path_for_absent_binary_errors() {
        let m = member("definitely-not-a-real-binary-xyz", ManageStrategy::OwnVerb);
        let err = apply_to_member(Verb::Start, &m).expect_err("absent binary errors");
        assert!(err.to_string().contains("not installed"));
    }
}
