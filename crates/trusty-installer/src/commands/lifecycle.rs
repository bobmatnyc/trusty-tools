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
//! drops the members not installed on this host (#10975), confirms the blast
//! radius (mirroring `install::run`), then applies the verb in
//! topological order (start = forward, stop = reverse, restart = bootout, wait
//! for launchd to finish the unload, then bootstrap per member). launchd members
//! use the shared `trusty_common::launchd::LaunchdConfig` `bootout`/`bootstrap`
//! (macOS-only), and restart routes through `restart_gracefully` so the two are
//! never issued back to back (#6618); `OwnVerb` members shell out to their own
//! subcommand.
//!
//! Test: `lifecycle_tests.rs` covers ordering, the report shaping/exit code, the
//! `Action` verb-name mapping, and (#10975) the not-installed filter driven
//! through a `ServiceEnv` fake; the actual launchctl/subprocess calls are
//! side-effecting and never invoked in tests.

use serde::Serialize;

use super::service_bootstrap::{RealServiceEnv, ServiceEnv};
use super::stable_set::{select_members, ManageStrategy, StableMember};
use crate::output::render_json;

/// #10975: the outcome detail for a member a bare verb skips because it is not
/// on this host. Info, not a failure: the outcome is `ok`.
const NOT_INSTALLED_NOTE: &str = "skipped (not installed)";

/// The lifecycle verb being applied.
///
/// Why: start/stop/restart share the same orchestration (resolve → confirm →
/// per-member apply) but differ in ordering and the underlying action; encoding
/// the verb as an enum keeps one code path with three small branch points.
/// What: `Start` (forward order, bring up), `Stop` (reverse order, take down),
/// `Restart` (bootout, wait for the unload, then bootstrap per member — #6618).
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

/// Shared orchestration for all three lifecycle verbs, on the real host.
///
/// Why: the three verbs share resolve → confirm → ordered-apply; one helper
/// avoids triplication (DRY).
/// What: [`run_lifecycle_in`] over [`RealServiceEnv`].
/// Test: `tests::bare_verbs_skip_members_that_are_not_installed` drives the
/// shared body through a fake.
fn run_lifecycle(verb: Verb, members: &[String], yes: bool, json: bool) -> i32 {
    run_lifecycle_in(&RealServiceEnv, verb, members, yes, json)
}

/// Is any trace of `binary` on this host — the binary, its plist, or its
/// loaded launchd label (#10975)?
///
/// Why: the stable set lists members this host may never have installed (an
/// internal trusty-events, an uninstalled trusty-search). Acting on one ran
/// `launchctl` for a unit that does not exist. Plist presence alone is the
/// wrong test: a binary without its plist still needs `start` to install the
/// service (#2556), and a loaded label whose plist is gone must reach
/// [`launchd_preflight`] so it errors instead of being skipped.
/// What: `true` when any of the three [`ServiceEnv`] signals holds.
/// Test: `tests::presence_reads_all_three_signals`.
fn member_installed(env: &dyn ServiceEnv, binary: &str) -> bool {
    env.binary_resolves(binary) || env.plist_present(binary) || env.is_loaded(binary)
}

/// Shared orchestration for all three lifecycle verbs, over a [`ServiceEnv`].
///
/// Why: the presence filter (#10975) reads the host, so a test needs to inject
/// a fake host to drive the whole verb path without touching launchd.
/// What: resolves members (unknown → exit 3), orders them per verb, then splits
/// off the members [`member_installed`] says are absent. A NAMED absent member
/// exits 2 with `<name>: not installed` before any action; with no member named,
/// each absent one gets an ok [`NOT_INSTALLED_NOTE`] outcome and is left out of
/// the prompt. Confirms the blast radius unless `--yes`/non-interactive/`--json`,
/// applies the verb to each present member via [`apply_to_member`], then renders
/// the report and returns its exit code.
/// Test: `tests::bare_verbs_skip_members_that_are_not_installed`,
/// `tests::named_member_not_installed_exits_2_with_no_action`,
/// `tests::start_installs_a_member_whose_plist_is_absent`,
/// `tests::loaded_label_without_plist_errors_before_bootout`.
fn run_lifecycle_in(
    env: &dyn ServiceEnv,
    verb: Verb,
    members: &[String],
    yes: bool,
    json: bool,
) -> i32 {
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

    // #10975: act only on members present on this host. Sits before the
    // prompt so the prompt lists only what will be acted on.
    let (present, absent): (Vec<StableMember>, Vec<StableMember>) = ordered
        .into_iter()
        .partition(|m| member_installed(env, &m.binary));
    if !members.is_empty() && !absent.is_empty() {
        let msg = absent
            .iter()
            .map(|m| format!("{}: not installed", m.crate_name))
            .collect::<Vec<_>>()
            .join("; ");
        if json {
            let _ = render_json(&serde_json::json!({ "command": verb.name(), "error": msg }));
        } else {
            eprintln!("tctl {}: {msg}", verb.name());
        }
        return 2;
    }

    if !present.is_empty() && !yes && !json && super::progress_ui::is_tty() {
        let names: Vec<&str> = present.iter().map(|m| m.crate_name.as_str()).collect();
        let q = format!(
            "{} {} daemon(s): {}? ",
            verb.name(),
            present.len(),
            names.join(", ")
        );
        if !super::progress_ui::prompt_yes_no(&q) {
            eprintln!("tctl {}: aborted (no confirmation).", verb.name());
            return 3;
        }
    }

    let skipped = absent.iter().map(|m| LifecycleOutcome {
        member: m.crate_name.clone(),
        ok: true,
        detail: NOT_INSTALLED_NOTE.to_owned(),
    });
    let applied = present.iter().map(|m| match apply_to_member(env, verb, m) {
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
    });
    let outcomes: Vec<LifecycleOutcome> = skipped.chain(applied).collect();

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

/// Bounce ONE member, connection-safely, by its lifecycle strategy (#4964).
///
/// Why: `tctl upgrade` places a new binary and then needs the running daemon to
/// actually start executing it. Before #4964 it called
/// `trusty_common::update::upgrade_and_restart`, whose restart mechanism is
/// `std::process::exit(1)` plus launchd `KeepAlive` — correct for the daemon
/// restarting ITSELF, and a guaranteed no-op when the caller is `tctl`, a
/// terminal process launchd does not supervise. Exposing the restart `tctl
/// restart` already performs, rather than writing a second one, is what keeps
/// the #4470 port guard, the bootout-before-bootstrap ordering, and the
/// `ExitTimeOut` drain window applying to upgrades too.
///
/// What: [`apply_to_member`] with [`Verb::Restart`] over [`RealServiceEnv`], for
/// a member synthesised from `binary` + `manage` (the only two fields the
/// restart path reads). #10975: [`launchd_preflight`] runs here too, so a
/// member installed with no plist gets a clear note, never launchd's error 5.
///
/// Test: `super::upgrade::tests::restart_plan_daemons_restart`,
/// `super::upgrade::tests::restart_plan_non_daemon_is_a_noop`,
/// `super::upgrade::tests::restart_plan_launchd_member_off_macos_is_manual`
/// pin the caller's dispatch (in `upgrade_tests.rs`); the
/// `launchctl`/subprocess calls are side-effecting and never run in unit
/// tests.
pub fn restart_member(binary: &str, manage: ManageStrategy) -> anyhow::Result<String> {
    let m = StableMember {
        crate_name: binary.to_owned(),
        binary: binary.to_owned(),
        daemon: manage != ManageStrategy::None,
        manage,
        required: true,
        soft: false,
        internal: false,
    };
    apply_to_member(&RealServiceEnv, Verb::Restart, &m)
}

/// Apply a lifecycle verb to a single member, dispatching on its strategy.
///
/// Why: launchd and process-managed members need different mechanisms; this is
/// the one place that branches on [`ManageStrategy`].
/// What: `Launchd` → [`launchd_preflight`], then [`launchd_control`] (macOS
/// `launchctl`) unless the preflight answered; `OwnVerb` → `<binary> <verb>`
/// subprocess; `None` → a no-op note, worded per whether the member is a
/// non-daemon or one whose service is retired (#6290, #6350). Returns a human
/// detail string on success.
/// Test: `tests::own_verb_path_for_absent_binary_errors`,
/// `tests::loaded_label_without_plist_errors_before_bootout`; the launchctl
/// calls themselves are side-effecting.
fn apply_to_member(env: &dyn ServiceEnv, verb: Verb, m: &StableMember) -> anyhow::Result<String> {
    match m.manage {
        ManageStrategy::Launchd => match launchd_preflight(env, verb, &m.binary)? {
            Some(note) => Ok(note),
            None => launchd_control(env, verb, &m.binary),
        },
        ManageStrategy::OwnVerb => own_verb_control(env, verb, &m.binary),
        // #6290, #6350: a member whose service is RETIRED lands here too, and
        // that is the point — reaching `launchd_control` would bootstrap the
        // very unit the retirement evicts. It is not a non-daemon, though, so
        // the note says which of the two it is.
        ManageStrategy::None if super::service_bootstrap::member_has_retired_service(&m.binary) => {
            Ok("skipped (launchd service retired; nothing supervises it)".to_owned())
        }
        ManageStrategy::None => Ok("skipped (not a daemon)".to_owned()),
    }
}

/// Decide, before any `launchctl` call, whether a stop or restart can proceed
/// for a launchd member whose plist is absent (#10975).
///
/// Why: with no plist, `restart_gracefully` boots the label out and then fails
/// to bootstrap it (`Bootstrap failed: 5: Input/output error`). If the label
/// was loaded, that bootout killed a running daemon that nothing can bring
/// back. `tctl upgrade` reaches the same path through [`restart_member`].
/// What: `Ok(None)` (proceed) for `Start` — its plist-absent case is the #2556
/// `service install` — and whenever the plist is present. With the plist
/// absent: a loaded label is an `Err` naming the missing plist; an unloaded one
/// is `Ok(Some(note))`, a skip with no `launchctl` call.
/// Test: `tests::loaded_label_without_plist_errors_before_bootout`,
/// `tests::preflight_skips_an_unloaded_member_without_plist`.
fn launchd_preflight(
    env: &dyn ServiceEnv,
    verb: Verb,
    binary: &str,
) -> anyhow::Result<Option<String>> {
    if verb == Verb::Start || env.plist_present(binary) {
        return Ok(None);
    }
    let label = super::plist_label::plist_label_for(binary);
    if env.is_loaded(binary) {
        anyhow::bail!(
            "{label} is loaded but its launchd plist is gone; refusing to boot it out, \
             because launchd could not bootstrap it again (`tctl start {binary}` \
             reinstalls the plist)"
        );
    }
    Ok(Some(format!(
        "skipped (no launchd plist for {label} and it is not loaded; \
         `tctl start {binary}` installs the service)"
    )))
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
///
/// 🔴 (#4470) Every branch that issues a `launchctl bootstrap` — or a `service
/// install`, which bootstraps — is gated first by
/// [`super::port_guard::guard_bootstrap`], the same check `tctl install` uses.
/// `bootstrap` exits 0 even when a foreign, unsupervised process already owns
/// the daemon's port, so an ungated one reports success while that process
/// keeps serving the port (#4230). For `Restart` the gate runs BEFORE the
/// `bootout`, so a refusal never stops a running daemon and then declines to
/// bring it back — the fail-open shape where the failure branch has already
/// advanced state.
/// Test: the `Start` plist-presence decision is pinned by
/// `super::service_bootstrap::tests::start_plan_maps_presence` and the guard
/// policy by `super::port_guard::tests`; the actual `launchctl`/subprocess
/// calls are side-effecting and never run in unit tests.
#[cfg(target_os = "macos")]
fn launchd_control(env: &dyn ServiceEnv, verb: Verb, binary: &str) -> anyhow::Result<String> {
    use super::service_bootstrap::{start_plan, StartPlan};
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
        working_directory: None,
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
            // #10975: read through the seam, so a fake host drives this branch.
            match start_plan(env.plist_present(binary)) {
                StartPlan::Bootstrap => {
                    // #2566 review (item 4): `bootstrap()` boots the agent OUT
                    // before reloading it — a redundant restart when `tctl
                    // start` runs right after `tctl install` already bootstrapped
                    // this same daemon seconds earlier. Skip the bounce when the
                    // agent is already loaded; this is a cheap `launchctl print`
                    // check (mirrors the daemons' own `service status`).
                    //
                    // #2573 (LOW): `is_loaded()` only confirms launchd
                    // REGISTRATION via `launchctl print` — it says nothing about
                    // whether the process is actually healthy, so a crash-looping
                    // agent (`KeepAlive::Always` restarting it every throttle
                    // interval) still reports "loaded" here. The message says
                    // "already loaded", not "already running", so it doesn't
                    // imply a health guarantee it can't back up.
                    if cfg.is_loaded() {
                        return Ok(format!("{} already loaded (skipped restart)", cfg.label));
                    }
                    // #4470: refuse rather than issue a bootstrap that would
                    // report success while a foreign process keeps the port.
                    env.port_guard(binary).map_err(anyhow::Error::msg)?;
                    cfg.bootstrap()?;
                    Ok(format!("bootstrapped {}", cfg.label))
                }
                StartPlan::ServiceInstall => {
                    // #4470: `service install` writes the plist AND bootstraps
                    // it — gate it before it can leave a half-installed member.
                    env.port_guard(binary).map_err(anyhow::Error::msg)?;
                    // #4964: `None` — `tctl start` installs no binary, so it has
                    // no concrete path to offer and keeps the `PATH` lookup.
                    env.run_service_install(binary, None).map_err(|e| {
                        anyhow::anyhow!(
                            "no launchd plist for {binary} and `service install` failed: {e}"
                        )
                    })?;
                    Ok(format!("installed + bootstrapped {}", cfg.label))
                }
            }
        }
        Verb::Restart => {
            // #4470: gate BEFORE the bootout, never after. Refusing after a
            // successful bootout would stop the daemon and then decline to
            // start it — a guard that makes things worse when it fires. A
            // correctly supervised daemon holds its own port here, so this
            // check passes for the ordinary restart.
            env.port_guard(binary).map_err(anyhow::Error::msg)?;
            // #6618: this used to be `bootout()` then `bootstrap()` back to
            // back. `launchctl bootout` returns when the unload is ACCEPTED,
            // not when the job is gone, so the bootstrap reached launchd while
            // the label was still registered and was refused with `Bootstrap
            // failed: 5: Input/output error`. `restart_gracefully` waits the
            // label out of launchd first, quiesces a unit whose loaded grace is
            // too short for the daemon's flush (the #6590 guard `service
            // install` already ran), and retries the bootstrap once after a
            // second wait. Its error still says the daemon WAS stopped, which
            // is the state change the operator's next action depends on.
            let restarted = cfg.restart_gracefully()?;
            Ok(format!(
                "restarted {} (waited {}s for launchd to unload; {} bootstrap attempt(s))",
                cfg.label,
                restarted.unload.waited_secs(),
                restarted.attempts
            ))
        }
    }
}

/// Non-macOS stub: launchd is unavailable, so launchd control is unsupported.
///
/// Why: the crate must compile on Linux/CI; launchd lifecycle is macOS-only.
/// What: always returns an error explaining the platform limitation.
/// Test: compile-only on non-macOS targets.
#[cfg(not(target_os = "macos"))]
fn launchd_control(_env: &dyn ServiceEnv, _verb: Verb, binary: &str) -> anyhow::Result<String> {
    anyhow::bail!("launchd control of `{binary}` is only supported on macOS")
}

/// Drive a process-managed member via its own `<binary> <verb>` subcommand.
///
/// Why: trusty-mpm is NOT launchd-managed — it ships `trusty-mpm start|stop|
/// restart` that spawn/`pkill` the daemon. Shelling to those is the only correct
/// way to control its lifecycle (#1332 decision 3).
/// What: spawns `<binary> <verb>`; maps a non-zero exit into an `Err`.
///
/// #4964: `binary` is resolved through `which::which`, so the process this
/// bounces is whichever copy wins `$PATH` — NOT necessarily the concrete path
/// `tctl upgrade` just wrote and health-gated before calling
/// [`restart_member`]. On a host with two copies on `PATH` the upgrade can
/// therefore restart a different binary than the one it verified. Left as-is
/// deliberately: the fix is one destination (Phase 3), not a second
/// path-threading change here.
///
/// Test: side-effecting subprocess; never exercised in unit tests.
fn own_verb_control(env: &dyn ServiceEnv, verb: Verb, binary: &str) -> anyhow::Result<String> {
    // #10975: presence through the seam, so a fake host never spawns the binary.
    if !env.binary_resolves(binary) {
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
#[path = "lifecycle_tests.rs"]
mod tests;
