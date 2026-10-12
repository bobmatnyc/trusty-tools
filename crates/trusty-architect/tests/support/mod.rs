//! #9171 A3b-1: shared fixtures for the wake tests: the prototype tests'
//! screen builders, the one tm-architect pane, and a scripted tmux.
//!
//! Each test binary uses a subset, so unused items are allowed here.
#![allow(dead_code)]

use std::path::Path;

use trusty_architect::poller::{Pane, Waker};
use trusty_architect::{FakeClock, PaneId, ScriptedTmux, TmuxError};

pub type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The prototype goldens' fixed clock: 2025-10-09T08:53:20.5Z.
pub const NOW: f64 = 1_760_000_000.5;
pub const ALERTS: &str = "/Users/x/inbox/alerts.jsonl";
pub const DIM: &str = "\u{1b}[2m";
pub const RESET: &str = "\u{1b}[0m";
pub const PM_TURN: [&str; 3] = [
    "⏺ Merged #12 and the CI run is green.",
    "",
    "  Next: the release notes.",
];

pub fn rule() -> String {
    "─".repeat(80)
}

fn turn() -> Vec<String> {
    PM_TURN.iter().map(|l| (*l).to_owned()).collect()
}

/// `screen(PM_TURN, box=text, dim=dim)` from the prototype's tests:
/// transcript, input box, footer.
pub fn screen(text: &str, dim: bool) -> String {
    let box_line = if dim {
        format!("❯\u{a0}{DIM}{text}{RESET}")
    } else {
        format!("❯\u{a0}{text}")
    };
    let mut lines = turn();
    lines.extend([rule(), box_line, rule()]);
    lines.extend([
        "  TM 1.7.4 ● | org/proj ⎇ tm-x | @me | Opus 5.5 | ctx 30% | $1.00 | ⏳30% 📅40% | 💸1%/1%"
            .to_owned(),
        "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 9 agents".to_owned(),
        format!("{}/rc", " ".repeat(70)),
        String::new(),
        "  ⏺ main".to_owned(),
    ]);
    lines.join("\n") + "\n\n"
}

/// `_wrapped_box_screen(PM_TURN, text)`: `text` split across two box rows,
/// the second row raw (`second` lets a test dim or decorate it).
pub fn wrapped_with(first: &str, second: &str) -> String {
    let mut lines = turn();
    lines.extend([rule(), format!("❯\u{a0}{first}"), second.to_owned(), rule()]);
    lines.join("\n") + "\n"
}

/// `_wrapped_box_screen(PM_TURN, text)`: halves by character count.
pub fn wrapped(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let (a, b) = chars.split_at(chars.len() / 2);
    wrapped_with(&a.iter().collect::<String>(), &b.iter().collect::<String>())
}

/// A pane row as `list_panes` reports it.
pub fn pane(session: &str, id: &str, active: &str) -> Result<Pane, TmuxError> {
    Ok(Pane {
        session: session.to_owned(),
        pane: PaneId::parse(id)?,
        cmd: "claude".to_owned(),
        active: active.to_owned(),
        path: String::new(),
        dead: "0".to_owned(),
    })
}

/// `SUP_PANE`: the one tm-architect pane, `%1`, active.
pub fn sup_pane() -> Result<Vec<Pane>, TmuxError> {
    Ok(vec![pane("tm-architect", "%1", "11")?])
}

/// A tmux whose owner check answers `tm-architect` and whose sends succeed.
pub fn owned_tmux() -> ScriptedTmux {
    ScriptedTmux::new()
        .fallback("display-message", "tm-architect\n")
        .fallback("send-keys", "")
}

pub fn waker<'a>(
    tmux: &'a ScriptedTmux,
    clock: &'a FakeClock,
    alerts: &'a str,
) -> Waker<'a, ScriptedTmux, FakeClock> {
    Waker {
        tmux,
        clock,
        alerts: Path::new(alerts),
        dry_run: false,
    }
}

/// The pointer `wake` types for `n` alerts, read off a dry run.
pub fn pointer(
    alerts: &str,
    n: u64,
    logged: Option<u64>,
) -> Result<String, Box<dyn std::error::Error>> {
    let tmux = ScriptedTmux::new().fallback("capture-pane", &screen("", false));
    let clock = FakeClock::new(NOW);
    let dry = Waker {
        dry_run: true,
        ..waker(&tmux, &clock, alerts)
    };
    let out = dry.wake(&sup_pane()?, n, logged)?;
    let msg = out.strip_prefix("dry-run: would send to %1: ");
    Ok(msg
        .ok_or_else(|| format!("not a dry run: {out}"))?
        .to_owned())
}

/// The `send-keys` argv tails (after `-t %N`), in call order.
pub fn sent(tmux: &ScriptedTmux) -> Vec<Vec<String>> {
    tmux.sends().into_iter().map(|c| c[3..].to_vec()).collect()
}
