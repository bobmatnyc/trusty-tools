//! `tm build-lease -- <command>` — run a heavy build under a machine-wide
//! build slot (#8261 increment two).
//!
//! Why: the `PreToolUse` hook rewrites every heavy build to this command, so
//! this is where the machine-wide cap is enforced for every session, agent and
//! subagent. See `trusty_mpm::core::build_lease` for the design and its
//! fail-open table.
//!
//! What: wait (bounded by `--wait-secs`, else `builders.lease_wait_secs`, both
//! clamped to 1..=600 s) for a slot. On admission, replace `CARGO_TARGET_DIR`
//! with the slot's pool directory only when it names the machine's SHARED
//! target directory; run the command with inherited stdio, forward
//! SIGINT/SIGTERM/SIGHUP to it, and exit with its status. The slot's `flock`
//! is held by this process until the command exits; if this process dies the
//! kernel releases it. When no lease can be taken at all, the build runs only
//! while the census admits it. On timeout, exit [`EXIT_LEASE_TIMEOUT`] naming
//! the holders and every reading. Every decision is POSTed to the daemon log.
//! Test: `tests/tm_build_lease.rs` (real processes and flocks).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use trusty_mpm::core::build_lease::EXIT_LEASE_TIMEOUT;
use trusty_mpm::core::build_lease::acquire::{AcquireParams, Outcome, acquire, acquire_unleased};
use trusty_mpm::core::build_lease::admission::Decision;
use trusty_mpm::core::build_lease::census::LiveSampler;
use trusty_mpm::core::build_lease::config::{BuildLeaseConfig, clamp_wait};
use trusty_mpm::core::build_lease::slots::{HolderRecord, SlotDir, SlotGuard};
use trusty_mpm::core::build_lease::stale_guard::{
    Invalidation, checkout_root, invalidate_if_checkout_changed, workspace_packages,
};
use trusty_mpm::core::build_lease::target_dir::{TargetDirChoice, choose, resolve_pool};
use trusty_mpm::core::builders::{BuildersConfig, resolve_max_concurrent};
use trusty_mpm::core::config::MpmConfig;

/// `tm build-lease` arguments.
///
/// Test: `cli_parses_build_lease_and_passes_the_exit_code_through`.
#[derive(Debug, Clone, clap::Args)]
pub(crate) struct BuildLeaseArgs {
    /// Seconds to wait for a slot; defaults to `builders.lease_wait_secs`.
    #[arg(long)]
    wait_secs: Option<u64>,
    /// The command to run, after `--`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
    command: Vec<String>,
}

/// Run `args.command` under a build slot and exit with its status.
///
/// Why: the verb's exit code IS its interface — the build's own code on
/// success, [`EXIT_LEASE_TIMEOUT`] when no slot freed — so it never returns.
/// Test: `tests/tm_build_lease.rs`.
pub(crate) async fn run(args: BuildLeaseArgs, url: Option<&str>) -> ! {
    let builders: BuildersConfig = MpmConfig::load_default().builders;
    let lease = BuildLeaseConfig::load_default();
    for warning in builders.deprecation_warnings() {
        eprintln!("tm build-lease: {warning}");
    }
    let url = trusty_mpm::core::discovery::resolve_daemon_url(url);
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // The checkout ROOT keys slot affinity and the stale-build guard.
    let checkout = checkout_root(&cwd).display().to_string();
    let command_line = shlex::try_join(args.command.iter().map(String::as_str))
        .unwrap_or_else(|_| args.command.join(" "));
    // Critic round 1: an explicit `--wait-secs` is clamped like the config key.
    let wait = args.wait_secs.map_or_else(
        || lease.effective_lease_wait(),
        |s| Duration::from_secs(clamp_wait(s)),
    );
    let params = AcquireParams::new(&builders, &lease, resolve_max_concurrent(), wait, &checkout);
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
    let outcome = tokio::task::block_in_place(|| match SlotDir::resolve(&home) {
        Ok(slots) => acquire(&slots, &params, &mut sampler, &mut on_wait),
        Err((first, second)) => acquire_unleased(
            &params,
            Instant::now() + wait,
            format!("no build-slot directory could be created ({first}; fallback: {second})"),
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
            let target = match target_dir(&builders, &home, &cwd, guard.slot()) {
                Ok(target) => target,
                Err(why) => {
                    // #8261 (owner ruling 2026-09-21): an unsafe target never admits.
                    drop(guard);
                    post_decision(&url, "refused-target", &command_line, &decision, &[]).await;
                    eprintln!("tm build-lease: not running the build (#8261) — {why}");
                    std::process::exit(EXIT_LEASE_TIMEOUT)
                }
            };
            let mut record =
                HolderRecord::new(guard.slot(), command_line.clone(), checkout.clone());
            record.target_dir.clone_from(&target);
            write_record(&mut guard, &record);
            post_decision(&url, "admitted", &command_line, &decision, &[]).await;
            let status = spawn_and_wait(
                &args.command,
                target.as_deref(),
                Some((&mut guard, &mut record)),
            )
            .await;
            drop(guard);
            exit_with(status)
        }
        Outcome::Unleased { why, decision } => {
            warn_degraded(&decision);
            eprintln!(
                "tm build-lease: WARNING running UNLEASED — {why}. The census admitted it \
                 ({} build(s) of ceiling {} already run without a lease); every other lease \
                 still sees it as a foreign build (#8261). `tm doctor` reports the fault.",
                decision.ceiling.saturating_sub(decision.n_effective),
                decision.ceiling
            );
            post_decision(&url, "admitted-unleased", &command_line, &decision, &[]).await;
            exit_with(spawn_and_wait(&args.command, None, None).await)
        }
        Outcome::TimedOut { decision, holders } => {
            post_decision(&url, "timed-out", &command_line, &decision, &holders).await;
            eprintln!(
                "tm build-lease: no build slot freed within {}s (#8261) — {} Re-run the same \
                 command to wait again; `tm doctor` shows the holders.",
                wait.as_secs(),
                render(&decision, &holders)
            );
            std::process::exit(EXIT_LEASE_TIMEOUT)
        }
        other => {
            eprintln!(
                "tm build-lease: unrecognised lease outcome {other:?}; not running the build"
            );
            std::process::exit(EXIT_LEASE_TIMEOUT)
        }
    }
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

/// The `CARGO_TARGET_DIR` for this build, seeding the slot directory if new.
///
/// What: `Ok(None)` leaves cargo's own choice; `Ok(Some(dir))` sets `dir`.
/// `Err` when the build would otherwise run in the machine's SHARED target
/// directory because its slot directory cannot be made — the cross-worktree
/// clobbering this lease exists to stop, so the caller refuses instead
/// (#8261, owner ruling 2026-09-21: an unsafe target never admits).
/// Test: `an_unusable_slot_directory_refuses_instead_of_sharing` in
/// `tests/tm_build_lease.rs`.
fn target_dir(
    builders: &BuildersConfig,
    home: &Path,
    cwd: &Path,
    slot: u32,
) -> Result<Option<String>, String> {
    let ambient = std::env::var("CARGO_TARGET_DIR").ok();
    let pool = resolve_pool(builders, home, cwd);
    let shared = pool.as_ref().ok().and_then(|(_, shared)| shared.clone());
    let slot_path = pool
        .as_ref()
        .map(|(p, _)| p.slot_path(slot))
        .map_err(Clone::clone);
    match choose(ambient.as_deref(), shared.as_deref(), slot_path) {
        TargetDirChoice::Keep(dir) => Ok(Some(dir)),
        TargetDirChoice::Slot(path) => {
            // `Slot` implies a resolved pool; see `choose`.
            let Ok((pool, shared)) = pool else {
                return Err(format!("the pool for {} vanished", path.display()));
            };
            let seeded = tokio::task::block_in_place(|| pool.seed(slot, shared.as_deref()));
            match seeded {
                Ok((dir, _)) => {
                    guard_against_stale_builds(&dir, cwd);
                    Ok(Some(dir.display().to_string()))
                }
                Err(err) => Err(format!(
                    "slot directory {} is unusable ({err}), and the inherited CARGO_TARGET_DIR \
                     is the machine's shared target directory, which concurrent worktrees \
                     overwrite. Repair the slot pool root (`builders.slot_pool_root`, see \
                     `tm doctor`) or set a private CARGO_TARGET_DIR for this build",
                    path.display()
                )),
            }
        }
        // Unset: cargo's own `./target` or a repo's `build.target-dir` stands.
        _ => Ok(ambient),
    }
}

/// Clear the slot's workspace fingerprints when it last built another checkout.
///
/// Why: see `trusty_mpm::core::build_lease::stale_guard` — without this a
/// slot shared in turn by two worktrees can report the second one "Fresh" and
/// hand back the first one's binary.
fn guard_against_stale_builds(slot_dir: &Path, cwd: &Path) {
    let root = checkout_root(cwd);
    let result = tokio::task::block_in_place(|| {
        invalidate_if_checkout_changed(slot_dir, &root, || workspace_packages(&root))
    });
    match result {
        Ok(Invalidation::Cleared {
            previous,
            removed,
            all,
        }) => eprintln!(
            "tm build-lease: slot {} last built {}; cleared {removed} {} fingerprint(s) so this \
             checkout's crates rebuild instead of reading as Fresh (#8261).",
            slot_dir.display(),
            previous.as_deref().unwrap_or("an unrecorded checkout"),
            if all {
                "(ALL — the package list was unreadable)"
            } else {
                "workspace-package"
            },
        ),
        Ok(_) => {}
        Err(err) => eprintln!("tm build-lease: WARNING {err}"),
    }
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
async fn spawn_and_wait(
    command: &[String],
    target_dir: Option<&str>,
    lease: Option<(&mut SlotGuard, &mut HolderRecord)>,
) -> std::io::Result<std::process::ExitStatus> {
    let Some((program, rest)) = command.split_first() else {
        return Err(std::io::Error::other("no command given after `--`"));
    };
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(rest);
    if let Some(dir) = target_dir {
        cmd.env("CARGO_TARGET_DIR", dir);
    }
    let mut child = cmd.spawn()?;
    let pid = child.id();
    if let (Some((guard, record)), Some(pid)) = (lease, pid) {
        record.child_pid = Some(pid);
        write_record(guard, record);
    }
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    let mut hup = signal(SignalKind::hangup())?;
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
async fn post_decision(
    url: &str,
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
    let sent = async {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(300))
            .timeout(Duration::from_millis(800))
            .build()?
            .post(format!("{url}/api/v1/build-lease/decisions"))
            .json(&body)
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
