//! Unit tests for the #8902 Architect pane floor (`architect_pane.rs`).
//!
//! The fixtures spell non-exact tmux targets on purpose: each is a command the
//! classifier judges, never a tmux call.

use super::super::architect_pane_probe::parse_panes;
use super::super::architect_pane_verbs::DENY_VERBS;
use super::*;

/// A probe over a fixed server: the Architect in `$1` (`tm-architect`, `%1`,
/// `@1`), the caller in `$2` (`pm`, `%2`, `@2`), the poller in `$3`
/// (`tm-architect-poll`, `%3`, `@3`).
struct Fake {
    live: Result<bool, String>,
    panes: Result<Vec<Pane>, String>,
    current: Option<String>,
}

fn pane(n: u8, session: u8, name: &str, architect: bool) -> Pane {
    Pane {
        pane: format!("%{n}"),
        window: format!("@{n}"),
        session: format!("${session}"),
        name: name.into(),
        architect,
        marked: false,
    }
}

fn fake() -> Fake {
    Fake {
        live: Ok(true),
        panes: Ok(vec![
            pane(1, 1, "tm-architect", true),
            pane(2, 2, "pm", false),
            pane(3, 3, "tm-architect-poll", false),
        ]),
        current: Some("%2".into()),
    }
}

impl PaneProbe for Fake {
    fn architect_live(&self) -> Result<bool, String> {
        self.live.clone()
    }
    fn panes(&self, server: &[String]) -> Result<Vec<Pane>, String> {
        // A private `-L other` server holds no Architect.
        if server.iter().any(|w| w == "other") {
            return Ok(vec![pane(9, 9, "tm-architect-test", false)]);
        }
        self.panes.clone()
    }
    fn current_pane(&self) -> Option<String> {
        self.current.clone()
    }
}

fn denied(probe: &Fake, command: &str) -> bool {
    evaluate_architect_pane(command, probe).is_some_and(|r| r.contains("#8902"))
}

/// Every spelling of a target that reaches the Architect's session.
const ARCHITECT_TARGETS: &[&str] = &[
    "=tm-architect",
    "=tm-architect:0.0",
    "tm-architect:0.0",
    "tm-arch",
    "%1",
    "@1",
    "'$1'",
    "'$1:0'",
    "=pm:%1",
];

/// Owner ruling 2026-09-29 12:20Z: every deny verb, in every target form,
/// from a non-Architect session; a source verb also through `-s`.
#[test]
fn every_deny_verb_is_denied_in_each_target_form() {
    let probe = fake();
    for verb in DENY_VERBS.iter().filter(|v| v.name != "kill-server") {
        let extra = if verb.name == "new-window" { " -k" } else { "" };
        for target in ARCHITECT_TARGETS {
            let command = format!("tmux {}{extra} -t {target}", verb.name);
            assert!(denied(&probe, &command), "{command}");
        }
        if verb.src {
            let command = format!("tmux {} -s %1 -t %2", verb.name);
            assert!(denied(&probe, &command), "{command}");
        }
    }
    for command in [
        "tmux kill-server",
        "tmux kill-session -a -t =pm",
        "tmux send -t %1 x",
        "tmux killp -t %1",
        "tmux respawn-p -k -t %1",
        "tmux neww -k -t =tm-architect:0",
        "tmux send-keys -lt %1 x",
    ] {
        assert!(denied(&probe, command), "{command}");
    }
}

/// While an Architect is live, a target or command the guard cannot resolve
/// counts as the Architect's (fail closed).
#[test]
fn an_unresolvable_target_denies_while_an_architect_is_live() {
    let probe = fake();
    for command in [
        "tmux send-keys -t \"$T\" x",
        "tmux send-keys -t $T x",
        "tmux kill-pane -t \"$(tmux display -p -t =pm: '#{pane_id}')\"",
        "tmux kill-pane -t `cat pane.txt`",
        "tmux kill-pane -t '{marked}'",
        "tmux kill-pane -t '~'",
        "tmux kill-pane -t '!'",
        "tmux kill-pane -t '+1'",
        "tmux kill-session -t 'tm-*'",
        "tmux kill-pane -t '#{pane_id}'",
        "tmux kill-pane -t '%1x'",
        "tmux send-keys -Q -t =pm x",
        "tmux send-keys -t",
        "tmux send-keys -c /dev/ttys001 -t =pm x",
        "tmux zap -t =pm",
        "tmux source-file ~/.tmux.conf",
        "tmux -C attach",
        "tmux $VERB -t =pm",
        "tmux send-keys -t 'x",
        "TMUX_PANE=%1 tmux send-keys x",
        "tmux has-session -t =pm ';' zap",
        r"$'\x74mux' kill-session -t =tm-architect",
    ] {
        assert!(denied(&probe, command), "{command}");
    }
    // No `-t` and no known current pane.
    let lost = Fake {
        current: None,
        ..fake()
    };
    assert!(denied(&lost, "tmux send-keys x Enter"));
    // The current pane is not on the Architect's server.
    let elsewhere = Fake {
        current: Some("%9".into()),
        ..fake()
    };
    assert!(denied(&elsewhere, "tmux send-keys x Enter"));
    // tmux will not list its panes.
    let broken = Fake {
        panes: Err("server exited unexpectedly".into()),
        ..fake()
    };
    assert!(denied(&broken, "tmux send-keys -t =pm x"));
}

/// No live Architect launch record: the rule does not apply. Records that
/// cannot be read count as a live Architect.
#[test]
fn no_live_architect_means_the_rule_does_not_apply() {
    let none = Fake {
        live: Ok(false),
        ..fake()
    };
    assert_eq!(
        evaluate_architect_pane("tmux kill-session -t =tm-architect", &none),
        None
    );
    let unreadable = Fake {
        live: Err("records unreadable".into()),
        ..fake()
    };
    assert!(denied(&unreadable, "tmux kill-session -t =tm-architect"));
}

/// Normal PM tmux use: its own pane, another session's pane, a read verb on
/// the Architect (#8258 capture), geometry-only verbs, and a server with no
/// Architect on it.
#[test]
fn a_non_architect_target_and_a_read_verb_pass() {
    let probe = fake();
    for command in [
        "tmux send-keys -t =pm:0 'Run the gates, then git push your branch' Enter",
        "tmux send-keys -t %2 'y' Enter",
        "tmux send-keys -t @2 n Enter",
        "tmux send-keys -t '$2' j",
        "tmux send-keys 'hello' Enter",
        "tmux kill-session -t =tm-architect-poll",
        "tmux capture-pane -p -t %2",
        "tmux capture-pane -p -t =tm-architect: -S -200",
        "tmux resize-pane -Z -t =tm-architect",
        "tmux select-layout -t %1 tiled",
        "tmux new-window -t =tm-architect:",
        "tmux -L other kill-server",
        "tmux has-session -t =tm-architect",
        "tmux n",
        "echo tmux kill-session",
        "tmux send-keys -t =pm x \\; send-keys -t =pm Enter",
    ] {
        assert_eq!(evaluate_architect_pane(command, &probe), None, "{command}");
    }
}

/// A tmux command reached through a wrapper, a substitution, `tmux -c`, a
/// tmux command string, `bind-key`, a `;` separator, an alias definition, or
/// keys typed into a shell pane.
#[test]
fn a_tmux_command_reached_through_a_wrapper_or_argument_is_found() {
    let probe = fake();
    for command in [
        "sh -c 'tmux kill-pane -t %1'",
        "echo $(tmux kill-pane -t %1)",
        "timeout 5 tmux killp -t %1",
        "/opt/homebrew/bin/tmux send -t %1 x",
        "tmux -c 'tmux kill-pane -t %1'",
        "tmux run-shell 'tmux kill-pane -t %1'",
        "tmux if-shell true 'kill-pane -t %1'",
        "tmux bind-key X kill-pane -t %1",
        "tmux new-window 'tmux kill-pane -t %1'",
        "tmux new-window \\; kill-pane -t %1",
        "tmux set -s command-alias[9] zap=kill-server",
        "tmux send-keys -t %2 'tmux kill-session -t =tm-architect' Enter",
    ] {
        assert!(denied(&probe, command), "{command}");
    }
}

/// The session is the unit: a marked Architect pane is the default source of
/// `swap-pane`, and an Architect window linked into another session makes
/// that session the Architect's.
#[test]
fn a_marked_or_linked_architect_pane_is_reached() {
    let mut marked = fake();
    if let Ok(panes) = marked.panes.as_mut() {
        panes[0].marked = true;
    }
    assert!(denied(&marked, "tmux swap-pane -t %2"));
    assert_eq!(
        evaluate_architect_pane("tmux swap-pane -t %2", &fake()),
        None
    );
    let mut linked = fake();
    if let Ok(panes) = linked.panes.as_mut() {
        panes.push(pane(1, 2, "pm", true));
    }
    assert!(denied(&linked, "tmux send-keys -t =pm:9 x"));
}

/// The live probe's listing: a pane whose pid is in the Architect lineage,
/// or whose session is `tm-architect`, is the Architect's.
#[test]
fn a_pane_listing_marks_the_architect_by_lineage_and_session() {
    let text =
        "%1\t@1\t$1\t100\t0\ttm-architect\n%2\t@2\t$2\t200\t1\tpm\n%3\t@3\t$3\t300\t0\tw x\n";
    let panes = parse_panes(text, &[300]).expect("parse");
    let marks: Vec<(bool, bool)> = panes.iter().map(|p| (p.architect, p.marked)).collect();
    assert_eq!(marks, [(true, false), (false, true), (true, false)]);
    assert_eq!(panes[2].name, "w x");
    assert!(parse_panes("%1\t@1\n", &[]).is_err());
}
