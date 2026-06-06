//! Event display formatting for the REPL.
//!
//! Why: While a task runs, the controller emits typed `Event` values on the
//! global broadcast bus (see `crate::events`). The REPL listens to those and
//! renders them in real time so the user can watch the PM think, delegate,
//! and finish — instead of staring at a blank prompt until the final reply
//! arrives.
//! What: `format_event` returns a styled string for each `Event` variant.
//! Returns `None` for events the REPL doesn't surface (e.g. `Ping`).
//! Test: `format_event_examples` spot-checks a couple of variants for
//! stable substrings; the actual color codes are best validated visually.

use nu_ansi_term::{Color, Style};

use crate::events::Event;

/// Render one event as a (possibly styled) line, ready to be printed to the
/// terminal. Returns `None` for events that should not appear in the REPL
/// (currently just `Ping`).
///
/// Why: Centralizing formatting keeps the streaming loop focused on plumbing.
/// Different terminals render the same ANSI codes differently; we always
/// include enough textual context (`[PM]`, `[DELEGATING]`, `✓`) that the
/// output is readable even if color is stripped.
pub fn format_event(event: &Event) -> Option<String> {
    let dim = Style::new().dimmed();
    let teal = Style::new().fg(Color::Rgb(0x2E, 0xC4, 0xB6)).bold();
    let red = Style::new().fg(Color::Red).bold();
    let green = Style::new().fg(Color::Green);

    match event {
        Event::PmThinking { text, .. } => Some(dim.paint(format!("  [PM] {text}")).to_string()),
        Event::PmDelegating {
            agent,
            task_preview,
            ..
        } => Some(format!("  [DELEGATING] {agent} ← \"{task_preview}\"")),
        Event::AgentSpawned { agent, .. } => {
            Some(dim.paint(format!("  [SPAWN] {agent}")).to_string())
        }
        Event::AgentMessage { agent, text, .. } => Some(format!("  [{agent}] {text}")),
        Event::ToolCalled { tool, preview, .. } => {
            Some(dim.paint(format!("  [TOOL] {tool}: {preview}")).to_string())
        }
        Event::ToolResult { .. } => None, // suppress to reduce noise
        Event::AgentDone { agent, status, .. } if status == "success" => {
            Some(green.paint(format!("  ✓ {agent} complete")).to_string())
        }
        Event::AgentDone { agent, status, .. } => Some(format!("  [{agent}] done ({status})")),
        Event::AgentFailed { agent, error, .. } => {
            Some(red.paint(format!("  [ERROR] {agent}: {error}")).to_string())
        }
        Event::SessionStarted { project, .. } => Some(
            dim.paint(format!("→ Session started ({project})"))
                .to_string(),
        ),
        Event::SessionDone { status, .. } if status == "success" => {
            Some(teal.paint("[DONE] ✓ Task complete".to_string()).to_string())
        }
        Event::SessionDone { status, .. } => Some(
            red.paint(format!("[FAILED] Task ended: {status}"))
                .to_string(),
        ),
        Event::SessionCancelled { .. } => Some(red.paint("[CANCELLED]".to_string()).to_string()),
        Event::PhaseStarted { phase, .. } => {
            Some(dim.paint(format!("  ▸ phase: {phase}")).to_string())
        }
        Event::PhaseDone { phase, status, .. } => Some(
            dim.paint(format!("  ◂ phase {phase} → {status}"))
                .to_string(),
        ),
        Event::PhaseSkipped { phase, persona, .. } => Some(
            dim.paint(format!("  ⊘ phase {phase} skipped ({persona})"))
                .to_string(),
        ),
        Event::PersonaDetected { persona, .. } => {
            Some(dim.paint(format!("  ◆ persona: {persona}")).to_string())
        }
        Event::Ping => None,
        // #199: LLM lifecycle + agent fine-grained events emit on the bus but
        // are suppressed from the REPL stream by default to reduce noise.
        // (UI consumers can re-enable via separate display logic.)
        Event::LlmRequested { .. }
        | Event::LlmResponded { .. }
        | Event::AgentStarted { .. }
        | Event::ReportGenerated { .. }
        | Event::AstOperation { .. } => None,
        // #371: render a one-line recap banner when a session recap is generated.
        Event::RecapGenerated { summary, .. } => {
            Some(dim.paint(format!("  ※ recap: {summary}")).to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_event_examples() {
        let s = format_event(&Event::PmThinking {
            session_id: "s".into(),
            text: "considering options".into(),
        })
        .unwrap();
        assert!(s.contains("[PM]"), "{s}");
        assert!(s.contains("considering options"), "{s}");

        let s = format_event(&Event::SessionDone {
            session_id: "s".into(),
            status: "success".into(),
        })
        .unwrap();
        assert!(s.contains("Task complete"), "{s}");

        // Ping is suppressed.
        assert!(format_event(&Event::Ping).is_none());
    }
}
