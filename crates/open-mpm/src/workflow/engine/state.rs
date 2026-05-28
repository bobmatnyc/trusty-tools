//! Persona phase-gating and progress-event emission for the workflow engine.
//!
//! Why: The engine needs two small, side-effect-y helpers that are easy to
//! reason about in isolation — which phases a persona opts out of, and how a
//! phase transition is broadcast to stderr + the in-process event bus. Keeping
//! them out of the executor keeps the phase loop focused on orchestration.
//! What: `phases_to_skip` returns a static slice of phase names per persona;
//! `emit_progress_event` writes a legacy `__OMPM_PROGRESS__` line plus a typed
//! `Event` to the in-process bus.
//! Test: `phases_to_skip_*` unit tests below.

use crate::events::{Event, emit};

/// #196: Return the list of workflow phase names a given persona opts out of.
///
/// Why: `[hacker]`, `[vibe-coder]`, and `[novice]` were previously inert in the
/// prescriptive workflow because the engine ran every phase regardless of the
/// detected persona. Centralising the per-persona skip rules here keeps the
/// behaviour discoverable in one place and makes it trivial to extend with
/// new personas without touching the phase loop.
/// What: A static slice per persona. The default `engineer` persona returns
/// an empty slice (no phases skipped), preserving full-pipeline behaviour and
/// backward compatibility for tasks without any persona tag.
/// Test: `phases_to_skip_*` unit tests below assert each persona's skip set.
pub(crate) fn phases_to_skip(persona: &str) -> &'static [&'static str] {
    match persona {
        // Hacker: code-only. Skip research (heavyweight), plan (Opus is too
        // slow for throwaway scripts), QA (no test suite for one-off scripts),
        // and docs (no README for throwaway code). Fixes t06 latency: prior
        // skip set kept `plan` running on Opus (~120s) for ~269s total runtime.
        "hacker" => &["research", "plan", "qa", "docs"],
        // Vibe-coder: prototype-fast. Skip everything except code so the user
        // sees working output immediately. Plan is also skipped because the
        // request is "iterate", not "design first".
        "vibe-coder" => &["research", "plan", "qa", "docs"],
        // Novice: full pipeline (verbose output is handled by the persona
        // skill pack injected into the agent's prompt, not by skipping phases).
        "novice" => &[],
        // Engineer (default) and any unknown persona: full pipeline.
        _ => &[],
    }
}

/// Emit a machine-readable progress event to stderr (#149).
///
/// Why: The HTTP API server (`src/api/server.rs`) spawns `open-mpm` as a
/// subprocess and only sees stdout (the final JSON envelope) and stderr
/// (logging). To surface live phase progress to the Tauri UI poller, we emit
/// a single-line, prefix-tagged JSON record on stderr per phase event so the
/// server can parse those lines out of the stream and update the stored
/// `PmResponse.phases_completed` in real time. Other stderr lines pass
/// through unchanged.
/// What: Writes `__OMPM_PROGRESS__ {<json>}\n` to stderr plus a typed
/// `Event::PhaseStarted` / `Event::PhaseDone` to the in-process bus.
/// Test: Indirect — exercised by the api/server `run_task` stream parser
/// and by the engine integration test path.
pub(crate) fn emit_progress_event(
    name: &str,
    status: &str,
    elapsed_secs: f32,
    cost_usd: f32,
    note: Option<&str>,
) {
    // Legacy line — preserved for backwards compatibility with older parent
    // binaries that only know how to parse `__OMPM_PROGRESS__`.
    let event = serde_json::json!({
        "name": name,
        "status": status,
        "elapsed_secs": elapsed_secs,
        "cost_usd": cost_usd,
        "note": note,
    });
    eprintln!("__OMPM_PROGRESS__ {event}");

    // #192 Phase B: also emit a typed `Event::PhaseStarted` /
    // `Event::PhaseDone` on stderr (and the local in-process bus) so SSE
    // subscribers in the parent API server get phase transitions in real
    // time. `OPEN_MPM_RUN_ID` is set by the workflow harness; fall back to
    // an empty session id so the event is still visible (it just won't be
    // filtered by session).
    let session_id = std::env::var("OPEN_MPM_RUN_ID").unwrap_or_default();
    let phase = name.to_string();
    let typed = if status == "running" {
        Event::PhaseStarted { session_id, phase }
    } else {
        Event::PhaseDone {
            session_id,
            phase,
            status: status.to_string(),
        }
    };
    emit(typed);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #196: `engineer` (default) gets the full pipeline — no phases skipped.
    #[test]
    fn phases_to_skip_engineer_is_empty() {
        assert_eq!(phases_to_skip("engineer"), &[] as &[&str]);
    }

    /// #196: Tasks without any persona tag fall through to `engineer`,
    /// preserving backward-compatible behaviour. We assert the unknown-arm
    /// path returns an empty slice so unknown personas don't accidentally
    /// drop phases.
    #[test]
    fn phases_to_skip_unknown_persona_is_empty() {
        assert_eq!(phases_to_skip(""), &[] as &[&str]);
        assert_eq!(phases_to_skip("rogue-persona"), &[] as &[&str]);
    }

    /// #196 + t06 fix: Hacker persona is code-only — research/plan/qa/docs
    /// are skipped. Plan runs on Opus (~120s) which is too heavyweight for
    /// throwaway scripts; skipping it brings hacker latency from ~269s to
    /// roughly the code+observe runtime.
    #[test]
    fn phases_to_skip_hacker() {
        let s = phases_to_skip("hacker");
        assert!(s.contains(&"research"), "hacker must skip research: {s:?}");
        assert!(s.contains(&"plan"), "hacker must skip plan: {s:?}");
        assert!(s.contains(&"qa"), "hacker must skip qa: {s:?}");
        assert!(s.contains(&"docs"), "hacker must skip docs: {s:?}");
        // The code phase MUST run for the hacker persona — that's the whole point.
        assert!(!s.contains(&"code"), "hacker must NOT skip code: {s:?}");
    }

    /// #196: Vibe-coder skips everything except code (and observe, which is
    /// always run if defined — observe is reporting, not gating).
    #[test]
    fn phases_to_skip_vibe_coder() {
        let s = phases_to_skip("vibe-coder");
        assert!(s.contains(&"research"));
        assert!(s.contains(&"plan"));
        assert!(s.contains(&"qa"));
        assert!(s.contains(&"docs"));
        assert!(!s.contains(&"code"), "vibe-coder must NOT skip code: {s:?}");
    }

    /// #196: Novice gets the full pipeline. Verbosity is delivered via the
    /// persona skill pack injected into the agent prompt, not by skipping
    /// phases. (Skipping QA for a learner would be actively harmful.)
    #[test]
    fn phases_to_skip_novice_is_empty() {
        assert_eq!(phases_to_skip("novice"), &[] as &[&str]);
    }
}
