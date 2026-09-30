//! `tm fleet init`'s daemon registration and sidecar prune (#8942 phase 3).
//!
//! Why: the Architect runs outside the daemon; registering it gives it a
//! protected record that `tm ls` lists (design §2). Its launch sidecars keep
//! its names protected from kill-by-name, so a crashed launch's sidecar must
//! go, but only when that launch is proven gone (ruling 2).
//! What: [`registration`] builds the request from the names init used; the
//! daemon refuses a helper name not derived from the Architect's.
//! [`register`] sends it over the daemon's unix socket and [`register_step`]
//! maps the answer: no daemon is a `NOT registered` warning and init still
//! succeeds; a refusal, a failed call, or an answer without the Architect's
//! record is a failed step. [`prune_step`] runs the core prune with the
//! process probes and `init`'s tmux probe.
//! Test: `a_registration_answer_maps_to_its_step`,
//! `the_registration_names_the_real_helper_sessions`,
//! `fleet_init_prunes_only_a_proven_stale_sidecar`.

use std::path::Path;

use trusty_mpm::client::DaemonClient;
use trusty_mpm::client::http_client::supervisor::RegisterCallError;
use trusty_mpm::core::architect_sidecar_prune::{self as prune, SidecarProbes};
use trusty_mpm::session_manager::supervisor_register::collector_session_name;
use trusty_mpm::session_manager::{RegistrationReport, SessionKind, SupervisorRegistration};

use super::launch::PaneState;
use super::session_name::SessionNames;
use super::{Probe, Step};

/// The registration for the Architect in `dir` under `names`.
///
/// Test: `the_registration_names_the_real_helper_sessions`.
pub(crate) fn registration(dir: &Path, names: &SessionNames) -> SupervisorRegistration {
    SupervisorRegistration {
        dir: dir.to_path_buf(),
        session: names.architect().to_owned(),
        // #8942: `<session>-poll`, the only poller name the daemon accepts.
        poll_session: Some(names.poll().to_owned()),
        collector_session: Some(collector_session_name(names.architect())),
    }
}

/// Register `reg` with the local daemon over its socket; see the module doc.
pub(crate) async fn register(reg: &SupervisorRegistration) -> Step {
    let outcome = match DaemonClient::from_resolved_socket() {
        Ok(client) => client.register_supervisor(reg).await,
        Err(e) => Err(RegisterCallError::Unreachable(format!("{e:#}"))),
    };
    register_step(reg, outcome)
}

/// The `init` step for one registration answer (#8942).
///
/// Why: fail closed — only an answer naming the Architect's own record says
/// "registered"; only "no daemon" is a warning.
/// Test: `a_registration_answer_maps_to_its_step`.
pub(crate) fn register_step(
    reg: &SupervisorRegistration,
    outcome: Result<RegistrationReport, RegisterCallError>,
) -> Step {
    match outcome {
        Ok(report) => match report.registered.first() {
            Some(first)
                if first.kind == SessionKind::Supervisor && first.tmux_name == reg.session =>
            {
                let helpers: Vec<&str> = report.registered[1..]
                    .iter()
                    .map(|r| r.tmux_name.as_str())
                    .collect();
                Step::Changed(format!(
                    "registered {} with the daemon as the Architect (record {}; helpers: {})",
                    reg.session,
                    first.id,
                    if helpers.is_empty() {
                        "none".to_owned()
                    } else {
                        helpers.join(", ")
                    }
                ))
            }
            _ => Step::Failed(format!(
                "registration: the daemon's answer does not name the Architect's record for {}: \
                 {report:?}",
                reg.session
            )),
        },
        Err(RegisterCallError::Unreachable(why)) => {
            let text = format!(
                "NOT registered with the daemon ({why}); `tm ls` will not list the Architect \
                 until `tm fleet init` runs again with the daemon up"
            );
            eprintln!("warning: {text}");
            Step::Skipped(text)
        }
        Err(e) => Step::Failed(format!("registration of {}: {e}", reg.session)),
    }
}

/// Prune the stale Architect sidecars under `home` (#8942, ruling 2).
///
/// What: `None` when nothing was pruned or kept for doubt; a `Changed` step
/// naming the pruned sessions; an `Unchanged` step naming what was kept for
/// doubt, or why the directory could not be read.
/// Test: `fleet_init_prunes_only_a_proven_stale_sidecar`.
pub(crate) fn prune_step(home: &Path, probe: Probe) -> Option<Step> {
    let session_exists = |name: &str| match (probe.pane)(name) {
        PaneState::Absent => Some(false),
        PaneState::Live(_) | PaneState::Dead(_) => Some(true),
        PaneState::Unknown(_) => None,
    };
    let probes = SidecarProbes {
        pid_alive: &prune::pid_alive,
        start_time: &prune::start_time,
        session_exists: &session_exists,
    };
    let root = home.join(".trusty-mpm");
    match prune::prune_stale_sidecars(&root, probes) {
        Err(why) => Some(Step::Unchanged(format!(
            "Architect sidecars kept: cannot list them ({why})"
        ))),
        Ok(report) if !report.pruned.is_empty() => Some(Step::Changed(format!(
            "pruned the stale Architect sidecar(s) of {}{}",
            report.pruned.join(", "),
            kept_note(&report.kept)
        ))),
        Ok(report) if !report.kept.is_empty() => Some(Step::Unchanged(format!(
            "Architect sidecars kept{}",
            kept_note(&report.kept)
        ))),
        Ok(_) => None,
    }
}

fn kept_note(kept: &[String]) -> String {
    if kept.is_empty() {
        String::new()
    } else {
        format!("; kept, not proven stale: {}", kept.join("; "))
    }
}

#[cfg(test)]
#[path = "register_tests.rs"]
mod tests;
