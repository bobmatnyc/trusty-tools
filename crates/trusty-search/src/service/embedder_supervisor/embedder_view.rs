//! The embedder state an agent sees on every search response (#9970).
//!
//! Why: owner ruling D1b — while the python embedder installs in the
//! background, the daemon's response to the agent carries a note — and the
//! 2026-08-11 principle that a degraded fallback is allowed but silent
//! degradation is not (Q5: an offline host or `TRUSTY_UV_FETCH=0` stays on
//! ort and says so).
//! What: [`EmbedderView`] names the active backend, the intended one and the
//! bootstrap step, plus one plain-language note whenever the two backends
//! differ. Search responses serialize it as `meta.embedder`; `/health` copies
//! `bootstrap` and `note` into `python_bootstrap`.
//! Test: the tests below; `search_carries_the_note_while_the_bootstrap_runs`
//! and siblings in `server/tests_9970_embedder_note.rs`.

use serde::Serialize;

use super::bootstrap_control::{rfc3339, BootstrapPhase, BootstrapReport};
use super::{ActiveBackend, BackendKind, BootstrapState, SwitchableEmbedder};

/// `BackendKind` → the backend string `/health` and `meta.embedder` report.
///
/// `Remote` covers both HTTP and UDS remotes; splitting it is deferred (PR-1
/// review, low priority).
/// Test: `backend_kind_str_maps_every_variant` in `server/health.rs`.
pub fn backend_kind_str(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Ort => "ort",
        BackendKind::Python => "python",
        BackendKind::Remote => "remote",
        BackendKind::InProcess => "in_process",
        BackendKind::Candle => "candle",
    }
}

/// `BootstrapState` → the `embedder_bootstrap` string `/health` reports.
/// Test: `bootstrap_state_str_maps_every_variant` in `server/health.rs`.
pub fn bootstrap_state_str(state: BootstrapState) -> &'static str {
    match state {
        BootstrapState::NotApplicable => "n/a",
        BootstrapState::Bootstrapping => "bootstrapping",
        BootstrapState::Ready => "ready",
        BootstrapState::Failed => "failed",
        BootstrapState::Retrying => "retrying",
        BootstrapState::FellBackToOrt => "fell_back_to_ort",
    }
}

/// The bootstrap step: `state`'s own string, refined to `installing_venv`
/// or `probing` while an attempt runs (startup or background).
fn bootstrap_step(state: BootstrapState, phase: BootstrapPhase) -> &'static str {
    let running = matches!(
        state,
        BootstrapState::Bootstrapping | BootstrapState::Retrying
    );
    match phase {
        BootstrapPhase::InstallingVenv if running => "installing_venv",
        BootstrapPhase::Probing if running => "probing",
        _ => bootstrap_state_str(state),
    }
}

/// Active backend, intended backend, bootstrap step and the agent note.
///
/// Why: see the module docs.
/// What: `intended` is `python` whenever a python bootstrap applies (any
/// state but `NotApplicable`, which only the graceful default and the
/// swap-back watchdog leave), else the active backend. `note` is `Some`
/// exactly when `active != intended`.
/// Test: `note_names_each_degraded_step`, `no_note_once_python_serves`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EmbedderView {
    /// The backend serving embeddings now.
    pub active: &'static str,
    /// The backend this host is configured or defaulted to.
    pub intended: &'static str,
    /// `n/a`, `bootstrapping`, `installing_venv`, `probing`, `retrying`,
    /// `failed`, `ready` or `fell_back_to_ort`.
    pub bootstrap: &'static str,
    /// What the agent should know about the results; `null` when the
    /// intended backend serves.
    pub note: Option<String>,
}

impl EmbedderView {
    /// The view of `switchable`'s current backend and bootstrap report.
    pub fn of(switchable: &SwitchableEmbedder) -> Self {
        Self::from_parts(
            &switchable.active(),
            &switchable.bootstrap_control().report(),
        )
    }

    /// The view of one `active` snapshot and one `report`.
    pub fn from_parts(active: &ActiveBackend, report: &BootstrapReport) -> Self {
        let intended = match active.bootstrap {
            BootstrapState::NotApplicable => active.kind,
            _ => BackendKind::Python,
        };
        let (active_str, intended) = (backend_kind_str(active.kind), backend_kind_str(intended));
        let bootstrap = bootstrap_step(active.bootstrap, report.phase);
        let note = (active_str != intended).then(|| note(active_str, bootstrap, report));
        Self {
            active: active_str,
            intended,
            bootstrap,
            note,
        }
    }
}

/// The note for a host serving on `active` while python is intended.
fn note(active: &str, bootstrap: &str, report: &BootstrapReport) -> String {
    let head = format!("Degraded: search results come from the {active} fallback embedder");
    let reason = report.last_error.as_deref().unwrap_or("no error recorded");
    match bootstrap {
        "installing_venv" => format!(
            "{head} while the python embedder installs in the background (the first \
             install downloads about 3 GB). The daemon switches to python by itself."
        ),
        "probing" => format!(
            "{head} while the python embedder starts and answers a test embed. The \
             daemon switches to python by itself when it passes."
        ),
        "retrying" => {
            let at = report
                .next_retry_at
                .map_or_else(|| "soon".to_string(), rfc3339);
            format!(
                "{head}: the python embedder failed to start ({reason}). The next \
                 attempt is at {at}."
            )
        }
        "failed" => format!(
            "{head} for the rest of this daemon's run: the python embedder setup failed \
             ({reason}). Fix the cause and restart the daemon, or set \
             TRUSTY_EMBEDDER=stdio to choose ort."
        ),
        "fell_back_to_ort" => format!(
            "{head}: the python embedder stopped and could not be restarted. Restart \
             the daemon to try python again."
        ),
        _ => format!("{head} while the python embedder bootstrap starts."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::embedder::ExecutionProvider;

    fn active(kind: BackendKind, bootstrap: BootstrapState) -> ActiveBackend {
        ActiveBackend {
            kind,
            provider: ExecutionProvider::Cpu,
            model: "all-MiniLM-L6-v2".to_string(),
            quantized: false,
            bootstrap,
        }
    }

    /// Each step python is not yet serving gets its own degraded note.
    #[test]
    fn note_names_each_degraded_step() {
        let failed = BootstrapReport {
            last_error: Some("TRUSTY_UV_FETCH=0 turns off fetching uv".to_string()),
            ..BootstrapReport::default()
        };
        let cases = [
            (
                BootstrapState::Bootstrapping,
                BootstrapPhase::InstallingVenv,
                "installing_venv",
                "installs",
            ),
            (
                BootstrapState::Bootstrapping,
                BootstrapPhase::Probing,
                "probing",
                "test embed",
            ),
            (
                BootstrapState::Retrying,
                BootstrapPhase::Idle,
                "retrying",
                "next attempt",
            ),
            (
                BootstrapState::Retrying,
                BootstrapPhase::InstallingVenv,
                "installing_venv",
                "installs",
            ),
            (
                BootstrapState::Failed,
                BootstrapPhase::Idle,
                "failed",
                "TRUSTY_UV_FETCH=0",
            ),
            (
                BootstrapState::FellBackToOrt,
                BootstrapPhase::Idle,
                "fell_back_to_ort",
                "restarted",
            ),
        ];
        for (state, phase, step, says) in cases {
            let report = BootstrapReport {
                phase,
                ..failed.clone()
            };
            let view = EmbedderView::from_parts(&active(BackendKind::Ort, state), &report);
            assert_eq!(
                (view.active, view.intended, view.bootstrap),
                ("ort", "python", step)
            );
            let note = view.note.expect("python is intended, ort serves");
            assert!(
                note.starts_with("Degraded:") && note.contains(says),
                "{step}: {note}"
            );
        }
    }

    /// Python serving, or no bootstrap applying, carries no note.
    #[test]
    fn no_note_once_python_serves() {
        let report = BootstrapReport::default();
        let swapped =
            EmbedderView::from_parts(&active(BackendKind::Python, BootstrapState::Ready), &report);
        assert_eq!(
            (swapped.active, swapped.intended, swapped.bootstrap),
            ("python", "python", "ready")
        );
        assert_eq!(swapped.note, None);
        let plain = EmbedderView::from_parts(
            &active(BackendKind::Ort, BootstrapState::NotApplicable),
            &report,
        );
        assert_eq!(
            (plain.intended, plain.bootstrap, plain.note),
            ("ort", "n/a", None)
        );
    }
}
