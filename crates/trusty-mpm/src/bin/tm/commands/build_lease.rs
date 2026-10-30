//! `tm build-lease -- <command>` — run a heavy build under a machine-wide
//! build slot (#8261 increment two).
//!
//! Why: the `PreToolUse` hook rewrites every heavy build to this command, so
//! this is where the machine-wide cap is enforced for every session, agent and
//! subagent. See `trusty_mpm::core::build_lease` for the design and its
//! fail-open table.
//!
//! What: refuse (exit [`EXIT_NOT_A_HEAVY_BUILD`]) any command the heavy-build
//! classifier would not have leased. Otherwise wait (bounded by `--wait-secs`,
//! else `builders.lease_wait_secs`, both clamped to 1..=600 s) for a slot. On
//! admission, replace `CARGO_TARGET_DIR` with the slot's pool directory when it
//! names a SHARED target directory, and name that directory on stderr; run the
//! command with inherited stdio, forward SIGINT/SIGTERM/SIGHUP to it, and exit
//! with its status. The slot's
//! `flock` is held by this process until the command exits; if this process
//! dies the kernel releases it. When no lease can be taken, the build runs
//! UNLEASED only while the census counts fewer than the ceiling (owner ruling
//! "allow up to the cap", #8261 round 3), with a degraded warning naming the
//! store, the OS error and the repair; an unreadable census refuses. On
//! timeout, exit [`EXIT_LEASE_TIMEOUT`] naming the holders and every reading. Every decision is POSTed to the daemon log with the
//! command summarized, never its full argv.
//!
//! `tm build-lease status` runs no build and takes no lease: it lists the
//! holders, each repo's slots, and every seed in progress or abandoned with
//! its slot, owner pid and age (#9239).
//!
//! `tm build-lease --census` runs no build; it probes slot locks like `tm
//! doctor` (#8261 round 5). It prints the lease holders and every build group
//! the census counts against the ceiling, attributed by pgid, leader, driver
//! and parent chain (#8261 round 4).
//! Test: `tests/tm_build_lease.rs` (real processes and flocks).

use std::path::{Path, PathBuf};
use std::time::Duration;

use trusty_mpm::core::build_lease::EXIT_LEASE_TIMEOUT;
use trusty_mpm::core::build_lease::acquire::{
    AcquireParams, LeaseFault, Outcome, acquire, acquire_unleased,
};
use trusty_mpm::core::build_lease::admission::Decision;
use trusty_mpm::core::build_lease::census::LiveSampler;
use trusty_mpm::core::build_lease::config::{BuildLeaseConfig, clamp_wait};
use trusty_mpm::core::build_lease::deps_rotate::{lease_notice, rotate_before_build};
use trusty_mpm::core::build_lease::slots::{HolderRecord, SlotDir, SlotGuard, summarize_command};
use trusty_mpm::core::build_lease::stale_guard::{cargo_lock_held, checkout_root};
use trusty_mpm::core::build_lease::target_dir::{
    SharedDirs, TargetPlan, explicit_target_dir_arg, plan_with_worktree, resolve_pool,
    rewrite_target_dir_arg,
};
use trusty_mpm::core::builders::{BuildersConfig, resolve_max_concurrent};
use trusty_mpm::core::harness_root::linked_worktree_owner;

use super::build_lease_status::{print_census, print_status};
use super::build_lease_target::{announce_slot, target_dir};
use super::pm_guard_bash::build_lease_rewrite::is_heavy_build;
use trusty_mpm::core::config::MpmConfig;

/// The exit code for a command that is not a heavy build (`EX_USAGE`).
const EXIT_NOT_A_HEAVY_BUILD: i32 = 64;

/// The exit code for a build outside the directory its command meant
/// (`EX_CONFIG`, #8969).
const EXIT_WRONG_DIRECTORY: i32 = 78;

/// Enter, or verify, the directory the hook resolved for the build (#8969).
///
/// Why: Claude Code can drop a leading `cd` and run the rest elsewhere, so the
/// lease — the last process before cargo — must never build another checkout.
/// What: `chdir` returns canonical `DIR` for the caller to run the build in
/// (never a process-wide `set_current_dir`, #5544), naming the move when the
/// shell stood elsewhere. `expect` compares the canonical cwd with canonical
/// `DIR` and returns `None`. `Err` on a mismatch or an unusable directory: the
/// caller refuses and the build never starts.
/// Test: `a_cd_prefixed_build_runs_in_its_directory_when_the_cd_is_lost_8969`,
/// `a_build_outside_its_expected_directory_is_refused_8969`.
fn pin_directory(chdir: Option<&Path>, expect: Option<&Path>) -> Result<Option<PathBuf>, String> {
    let here = std::env::current_dir().and_then(|d| d.canonicalize());
    let canonical = |dir: &Path| {
        dir.canonicalize()
            .ok()
            .filter(|d| d.is_dir())
            .ok_or_else(|| format!("the command's directory {} is unusable", dir.display()))
    };
    if let Some(dir) = chdir {
        let dir = canonical(dir)?;
        if here.as_ref().ok() != Some(&dir) {
            eprintln!(
                "tm build-lease: building in {} — the command's `cd`; the shell started in {} \
                 (#8969)",
                dir.display(),
                here.as_ref().map_or_else(
                    |e| format!("an unreadable directory ({e})"),
                    |h| h.display().to_string()
                )
            );
        }
        return Ok(Some(dir));
    }
    let Some(want) = expect else {
        return Ok(None);
    };
    let want = canonical(want)?;
    let here = here.map_err(|e| format!("this process's directory is unreadable: {e}"))?;
    if here == want {
        return Ok(None);
    }
    Err(format!(
        "the command meant to build in {}, but the shell ran it in {}, so cargo would build a \
         different checkout. Re-run it with an absolute path, `cd {} && …`, or pass `cargo \
         --manifest-path <dir>/Cargo.toml`",
        want.display(),
        here.display(),
        want.display()
    ))
}

/// `tm build-lease` arguments.
///
/// Test: `cli_parses_build_lease_and_passes_the_exit_code_through`.
#[derive(Debug, Clone, clap::Args)]
pub(crate) struct BuildLeaseArgs {
    /// Seconds to wait for a slot; defaults to `builders.lease_wait_secs`.
    #[arg(long)]
    wait_secs: Option<u64>,
    /// Print the lease holders and the census, per build group, and exit.
    // #8261: round 4 — the live check logs this while builds run.
    #[arg(long, conflicts_with_all = ["command", "wait_secs", "chdir", "expect_cwd"])]
    census: bool,
    /// Run the build in DIR: the command's own absolute `cd`, which the shell
    /// may never see (#8969).
    #[arg(long, value_name = "DIR", conflicts_with = "expect_cwd")]
    chdir: Option<PathBuf>,
    /// Refuse unless this process runs in DIR, the directory the hook resolved
    /// for the command's `cd` (#8969).
    #[arg(long, value_name = "DIR")]
    expect_cwd: Option<PathBuf>,
    /// The heavy build to run, after `--`; anything else is refused. The
    /// single word `status` prints the lease and seeding status instead (#9239).
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        required_unless_present = "census"
    )]
    command: Vec<String>,
}

/// Run `args.command` under a build slot and exit with its status.
///
/// Why: the verb's exit code IS its interface — the build's own code on
/// success, [`EXIT_LEASE_TIMEOUT`] when no slot freed — so it never returns.
/// Test: `tests/tm_build_lease.rs`.
pub(crate) async fn run(args: BuildLeaseArgs) -> ! {
    if args.census {
        print_census()
    }
    // #9239: `tm build-lease status` is a read-only view; no lease, no build.
    if args.command == ["status"] {
        print_status(&MpmConfig::load_default().builders)
    }
    let builders: BuildersConfig = MpmConfig::load_default().builders;
    let lease = BuildLeaseConfig::load_default();
    let command_line = summarize_command(&args.command);
    // #8261 round 3 (critic finding 4): the lease program never runs anything
    // the hook's classifier would not have leased.
    if !is_heavy_build(&args.command, &lease.effective_heavy_build_commands()) {
        eprintln!(
            "tm build-lease: refusing `{command_line}` — it is not a heavy build \
             (`builders.heavy_build_commands`), and the lease runs heavy builds only. Run it \
             without `tm build-lease`."
        );
        std::process::exit(EXIT_NOT_A_HEAVY_BUILD)
    }
    // #8969: settle the build's directory before anything below reads the cwd.
    let pinned = match pin_directory(args.chdir.as_deref(), args.expect_cwd.as_deref()) {
        Ok(pinned) => pinned,
        Err(why) => {
            eprintln!("tm build-lease: not running `{command_line}` (#8969) — {why}");
            std::process::exit(EXIT_WRONG_DIRECTORY)
        }
    };
    for warning in builders.deprecation_warnings() {
        eprintln!("tm build-lease: {warning}");
    }
    // #6288 step 1: the decision log goes over the daemon socket only; a
    // socket that cannot be resolved costs the log line, never the build.
    let daemon = trusty_mpm::client::DaemonClient::from_resolved_socket()
        .inspect_err(|e| eprintln!("tm build-lease: no daemon socket for the decision log: {e:#}"))
        .ok();
    let home = dirs::home_dir();
    let home_or_root = home.clone().unwrap_or_else(|| PathBuf::from("/"));
    let cwd = pinned
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    // The checkout ROOT keys slot affinity and the stale-build guard.
    let checkout = checkout_root(&cwd).display().to_string();
    // Critic round 1: an explicit `--wait-secs` is clamped like the config key.
    let wait = args.wait_secs.map_or_else(
        || lease.effective_lease_wait(),
        |s| Duration::from_secs(clamp_wait(s)),
    );
    // #8261 round 6 (critic LOW): an explicit `--target-dir` in argv is the
    // value cargo actually uses — it outranks `CARGO_TARGET_DIR` from the
    // environment, so `plan` must see that value, not the env var, when both
    // are present.
    let explicit_target_dir = explicit_target_dir_arg(&args.command).map(str::to_string);
    let ambient = explicit_target_dir
        .clone()
        .or_else(|| std::env::var("CARGO_TARGET_DIR").ok());
    // #10215: an unset CARGO_TARGET_DIR in a linked worktree takes a slot too.
    let target_plan = plan_with_worktree(
        ambient.as_deref(),
        &SharedDirs::resolve(&builders, &home_or_root),
        resolve_pool(&builders, &home_or_root, &cwd),
        linked_worktree_owner(&cwd).is_some(),
    );
    if let TargetPlan::Refuse(why) = &target_plan {
        refuse(&format!("not running the build (#8261) — {why}"))
    }
    let slots = SlotDir::resolve(home.as_deref());
    if let Ok(Some(why)) = slots.as_ref().map(SlotDir::fallback_reason) {
        eprintln!(
            "tm build-lease: WARNING using the fallback lease store {} because {why}; \
             `tm doctor` names the repair",
            slots
                .as_ref()
                .map(|s| s.path().display().to_string())
                .unwrap_or_default()
        );
    }
    // #8261 round 3: a free slot whose directory an orphaned build still uses
    // (cargo's `.cargo-lock` held) is skipped.
    let busy = |slot: u32| match &target_plan {
        TargetPlan::Slot { pool, .. } | TargetPlan::UnsetSlot { pool, .. } => {
            cargo_lock_held(&pool.slot_path(slot))
        }
        _ => false,
    };
    let params = AcquireParams::new(&builders, &lease, resolve_max_concurrent(), wait, &checkout)
        .with_slot_busy(&busy);
    let mut sampler = LiveSampler::new(&lease);
    let mut announced = false;
    let mut on_wait = |decision: &Decision, holders: &[HolderRecord]| {
        if !announced {
            announced = true;
            eprintln!(
                "tm build-lease: waiting for a build slot — {}",
                render(decision, holders)
            );
        }
    };
    let outcome = tokio::task::block_in_place(|| match &slots {
        Ok(slots) => acquire(slots, &params, &mut sampler, &mut on_wait),
        Err(err) => acquire_unleased(
            &params,
            std::time::Instant::now() + wait,
            LeaseFault::new(
                err.canonical.clone(),
                format!(
                    "{}; fallback {}: {}",
                    err.canonical_error,
                    err.fallback.display(),
                    err.fallback_error
                ),
                err.repair(),
            ),
            &mut sampler,
            &mut on_wait,
        ),
    });
    match outcome {
        Outcome::Leased {
            mut guard,
            decision,
        } => {
            warn_degraded(&decision);
            let target = match target_dir(&target_plan, guard.slot(), &cwd, wait) {
                Ok(target) => target,
                Err(why) => {
                    // #8261 (owner ruling 2026-09-21): an unsafe target never admits.
                    drop(guard);
                    post_decision(
                        daemon.as_ref(),
                        "refused-target",
                        &command_line,
                        &decision,
                        &[],
                    )
                    .await;
                    refuse(&format!("not running the build (#8261) — {why}"))
                }
            };
            announce_slot(&target_plan, guard.slot(), target.as_deref());
            // #10212: the slot's flock is held here, so no other lease builds in it.
            if let Some(dir) = target.as_deref().map(Path::new)
                && let Some(line) = lease_notice(dir, &rotate_before_build(dir))
            {
                eprintln!("tm build-lease: {line}");
            }
            let mut record =
                HolderRecord::new(guard.slot(), command_line.clone(), checkout.clone());
            record.target_dir.clone_from(&target);
            write_record(&mut guard, &record);
            post_decision(daemon.as_ref(), "admitted", &command_line, &decision, &[]).await;
            // #8736: cargo run/watch slot handling is tracked in #8692
            // #8261 round 6 (critic LOW): an explicit `--target-dir` in argv
            // outranks `CARGO_TARGET_DIR`, so it must carry the slot too.
            let run_argv = match (&explicit_target_dir, &target) {
                (Some(_), Some(dir)) => rewrite_target_dir_arg(&args.command, dir),
                _ => args.command.clone(),
            };
            let status = spawn_and_wait(
                &run_argv,
                target.as_deref(),
                pinned.as_deref(),
                Some((&mut guard, &mut record)),
            )
            .await;
            drop(guard);
            exit_with(status)
        }
        Outcome::Unleased { fault, decision } => {
            if let TargetPlan::Slot { .. } = target_plan {
                // #8261 (owner ruling 2026-09-21): no slot, so no private
                // directory can replace the shared one — never build there.
                refuse(&format!(
                    "not running the build (#8261) — no build lease can be taken ({}), and \
                     the inherited CARGO_TARGET_DIR is a shared target directory that only a \
                     leased slot can replace",
                    fault.render()
                ))
            }
            eprintln!(
                "tm build-lease: WARNING DEGRADED — running UNLEASED: no build lease can be \
                 taken in {}. The census admitted it ({} build(s) of ceiling {} already run \
                 without a lease); every other lease still counts it as a foreign build \
                 (#8261). `tm doctor` shows the builder_cap row.",
                fault.render(),
                decision.ceiling.saturating_sub(decision.n_effective),
                decision.ceiling
            );
            post_decision(
                daemon.as_ref(),
                "admitted-unleased",
                &command_line,
                &decision,
                &[],
            )
            .await;
            exit_with(spawn_and_wait(&args.command, None, pinned.as_deref(), None).await)
        }
        Outcome::TimedOut { decision, holders } => {
            post_decision(
                daemon.as_ref(),
                "timed-out",
                &command_line,
                &decision,
                &holders,
            )
            .await;
            eprintln!(
                "tm build-lease: no build slot freed within {}s (#8261) — {} Re-run the same \
                 command to wait again; `tm doctor` shows the holders.",
                wait.as_secs(),
                render(&decision, &holders)
            );
            std::process::exit(EXIT_LEASE_TIMEOUT)
        }
        other => refuse(&format!(
            "unrecognised lease outcome {other:?}; not running the build"
        )),
    }
}

/// Print `tm build-lease: <why>` and exit [`EXIT_LEASE_TIMEOUT`].
fn refuse(why: &str) -> ! {
    eprintln!("tm build-lease: {why}");
    std::process::exit(EXIT_LEASE_TIMEOUT)
}

fn warn_degraded(decision: &Decision) {
    for line in &decision.degraded {
        eprintln!("tm build-lease: WARNING {line}");
    }
}

/// One line naming why, the readings, the holders and the ceiling.
fn render(decision: &Decision, holders: &[HolderRecord]) -> String {
    let why = if decision.withheld.is_empty() {
        "no slot was free".to_string()
    } else {
        decision.withheld.join("; ")
    };
    let held = if holders.is_empty() {
        "none".to_string()
    } else {
        holders
            .iter()
            .map(HolderRecord::render)
            .collect::<Vec<_>>()
            .join("; ")
    };
    let degraded = if decision.degraded.is_empty() {
        String::new()
    } else {
        format!(" Degraded: {}.", decision.degraded.join("; "))
    };
    format!(
        "{why}. Readings: {}. Holders: {held}. Slots: {} effective of ceiling {} \
         (builders.max_concurrent).{degraded}",
        decision.readings, decision.n_effective, decision.ceiling
    )
}

fn write_record(guard: &mut SlotGuard, record: &HolderRecord) {
    if let Err(err) = guard.write_record(record) {
        eprintln!(
            "tm build-lease: could not write the holder record to {}: {err}",
            guard.path().display()
        );
    }
}

/// Spawn the build, record its pid, forward termination signals, and wait.
///
/// What: the signal handlers are installed BEFORE the spawn (#8261 round 3),
/// so a SIGTERM that lands between the spawn and the handler cannot kill this
/// holder with the default action and orphan the build.
/// `dir` is the `--chdir` directory the build runs in (#8969).
async fn spawn_and_wait(
    command: &[String],
    target_dir: Option<&str>,
    dir: Option<&Path>,
    lease: Option<(&mut SlotGuard, &mut HolderRecord)>,
) -> std::io::Result<std::process::ExitStatus> {
    let Some((program, rest)) = command.split_first() else {
        return Err(std::io::Error::other("no command given after `--`"));
    };
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    let mut hup = signal(SignalKind::hangup())?;
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(rest);
    if let Some(dir) = dir {
        cmd.current_dir(dir).env("PWD", dir);
    }
    if let Some(dir) = target_dir {
        cmd.env("CARGO_TARGET_DIR", dir);
    }
    // #8736: a SIGKILL between spawn and the child_pid write leaves the slot Free; waived as unreachable, tracked in #8692
    let mut child = cmd.spawn()?;
    let pid = child.id();
    if let (Some((guard, record)), Some(pid)) = (lease, pid) {
        record.child_pid = Some(pid);
        write_record(guard, record);
    }
    loop {
        let sig = tokio::select! {
            status = child.wait() => return status,
            _ = term.recv() => libc::SIGTERM,
            _ = int.recv() => libc::SIGINT,
            _ = hup.recv() => libc::SIGHUP,
        };
        if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
            // SAFETY: kill(2) on the child's pid with a valid signal number.
            unsafe { libc::kill(pid, sig) };
        }
    }
}

/// Exit with the build's status: its code, or 128 + the signal that ended it.
fn exit_with(status: std::io::Result<std::process::ExitStatus>) -> ! {
    use std::os::unix::process::ExitStatusExt;
    match status {
        Ok(status) => std::process::exit(
            status
                .code()
                .or_else(|| status.signal().map(|s| 128 + s))
                .unwrap_or(1),
        ),
        Err(err) => {
            eprintln!("tm build-lease: could not run the command: {err}");
            std::process::exit(127)
        }
    }
}

/// Log one admission decision in the daemon, best effort.
///
/// Why: "the daemon logs every admission decision with its readings" (#8261).
/// The decision itself is local; a daemon that is down costs only this line,
/// and says so on stderr.
/// What: #6288 step 1 — `mpm.build_lease.decision` over the daemon's unix
/// socket, bounded at 800 ms. Best effort stays best effort, and there is no
/// TCP fallback: a socket error is reported on stderr naming the socket.
/// Test: `tm_build_lease_logs_its_decision_over_the_socket`.
async fn post_decision(
    daemon: Option<&trusty_mpm::client::DaemonClient>,
    verdict: &str,
    command: &str,
    decision: &Decision,
    holders: &[HolderRecord],
) {
    let body = serde_json::json!({
        "verdict": verdict,
        "command": command,
        "pid": std::process::id(),
        "readings": decision.readings,
        "withheld": decision.withheld,
        "degraded": decision.degraded,
        "n_effective": decision.n_effective,
        "ceiling": decision.ceiling,
        "held": decision.held,
        "holders": holders.iter().map(HolderRecord::render).collect::<Vec<_>>(),
    });
    let Some(daemon) = daemon else {
        return;
    };
    let sent = async {
        daemon
            .post("/api/v1/build-lease/decisions")
            .json(&body)
            .timeout(Duration::from_millis(800))
            .send()
            .await?
            .error_for_status()
    }
    .await;
    if let Err(err) = sent {
        eprintln!(
            "tm build-lease: the daemon did not log this {verdict} decision ({err}); the \
             decision stands — it is made locally."
        );
    }
}
