//! Unit tests for the #8902 Architect pane floor (`architect_pane.rs`).
//!
//! The fixtures spell non-exact tmux targets on purpose: each is a command the
//! classifier judges, never a tmux call.

use super::super::architect_pane_probe::{Listed, architect_marks, classify_listing, parse_panes};
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
        // A private `-L other` or `-L scratch` server holds no Architect; an
        // `-L arch` server holds the one `fake()` lists.
        if server.iter().any(|w| w == "other" || w == "scratch") {
            return Ok(vec![pane(9, 9, "tm-architect-test", false)]);
        }
        if server.iter().any(|w| w == "arch") {
            return fake().panes;
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
        // #8902 review: every wrapper the shared program resolver knows.
        "setsid tmux killp -t %1",
        "noglob tmux killp -t %1",
        "nocorrect tmux killp -t %1",
        "chrt 10 tmux killp -t %1",
        "taskset 1 tmux killp -t %1",
        "flock /tmp/l tmux killp -t %1",
        "unbuffer tmux killp -t %1",
        "gtimeout 5 tmux killp -t %1",
        "timeout 60 bash -c 'tmux kill-pane -t %1'",
        "nice -n 5 sh -c 'tmux kill-pane -t %1'",
        "env tmux killp -t %1",
        "command tmux killp -t %1",
        "nice tmux killp -t %1",
        "nice -n 5 tmux killp -t %1",
        "sudo -u x tmux killp -t %1",
    ] {
        assert!(denied(&probe, command), "{command}");
    }
}

/// #8902 review: the probe lists the server the hook process sees. A command
/// that picks its server another way — a relative `-S`, a changed `TMUX` or
/// `TMUX_TMPDIR`, or a wrapper that resets the environment — targets a server
/// the guard cannot resolve.
#[test]
fn a_server_the_command_selects_differently_denies() {
    let probe = fake();
    for command in [
        "tmux -S default kill-session -t =pm",
        "tmux -Sdefault kill-session -t =pm",
        "cd /private/tmp/tmux-501 && tmux -S default kill-session -t =tm-architect",
        "TMUX= tmux kill-session -t =pm",
        "TMUX=/tmp/tmux-501/default,1,0 tmux kill-session -t =pm",
        "env -u TMUX tmux kill-session -t =pm",
        "env -uTMUX tmux kill-session -t =pm",
        "env --unset=TMUX tmux kill-session -t =pm",
        "unset TMUX; tmux kill-session -t =pm",
        "export TMUX_TMPDIR=/tmp/elsewhere; tmux kill-session -t =pm",
        "TMUX_TMPDIR=/tmp/elsewhere tmux kill-session -t =pm",
        "env -i tmux kill-session -t =pm",
        "env - tmux kill-session -t =pm",
        "sudo tmux kill-session -t =pm",
        "doas tmux kill-session -t =pm",
        "sh -c 'unset TMUX; tmux kill-session -t =pm'",
        // #8902 MEDIUM-1: an empty environment, and a default that assigns.
        // `=pm:` needs no current pane, so only the server rule can deny.
        "exec -c tmux kill-session -t =pm:",
        ": \"${TMUX:=/tmp/x/default,1,0}\"; tmux kill-session -t =pm:",
        ": ${TMUX=/tmp/x/default,1,0}; tmux kill-session -t =pm:",
    ] {
        assert!(denied(&probe, command), "{command}");
    }
    for command in [
        "tmux -S /tmp/tmux-501/default capture-pane -p -t =tm-architect:",
        "echo \"$TMUX\"; tmux send-keys -t =pm: x",
        "TMUX= tmux ls",
        "sudo tmux ls",
    ] {
        assert_eq!(evaluate_architect_pane(command, &probe), None, "{command}");
    }
}

/// #8902 review: targets resolve against the panes before the command runs,
/// so a session the same command renames or creates, and a nested command
/// whose default target is the pane it runs in, cannot be resolved.
#[test]
fn a_target_the_same_command_retargets_denies() {
    let probe = fake();
    for command in [
        "tmux rename-session -t =tm-architect x \\; send-keys -t =x hi Enter",
        "tmux new-session -d -s g -t =tm-architect \\; send-keys -t =g hi Enter",
        "tmux set-hook -t =tm-architect after-resize-pane 'send-keys hi Enter' \\; \
         resize-pane -t =tm-architect -x 80",
        "tmux rename -t =tm-architect x; tmux send-keys -t =x hi Enter",
        "tmux new -d -s g -t =tm-architect && tmux kill-session -t =g",
        "tmux bind-key X kill-pane",
        "tmux run-shell 'tmux send-keys hi Enter'",
    ] {
        assert!(denied(&probe, command), "{command}");
    }
    for command in [
        "tmux new-session -d -s work",
        "tmux rename-session -t =pm pm2",
        "tmux send-keys -t =pm 'please send the report' Enter",
    ] {
        assert_eq!(evaluate_architect_pane(command, &probe), None, "{command}");
    }
}

/// #8902 MEDIUM-1: a nested `tmux` with no `-L`/`-S` runs with the `TMUX` of
/// the server that runs it — typed keys and commands a server runs — so it
/// reaches that server. The caller's own `tmux -c` shell does not.
#[test]
fn a_nested_invocation_inherits_the_outer_server() {
    // The default server holds no Architect; `-L arch` does.
    let probe = Fake {
        panes: Ok(vec![pane(2, 2, "pm", false), pane(3, 3, "w", false)]),
        ..fake()
    };
    for command in [
        "tmux -L arch run-shell 'tmux kill-pane -t %1'",
        "tmux -L arch send-keys -t =pm 'tmux kill-session -t =tm-architect' Enter",
    ] {
        assert!(denied(&probe, command), "{command}");
    }
    for command in [
        "tmux run-shell 'tmux kill-pane -t %1'",
        "tmux -L arch run-shell 'tmux -L other kill-server'",
        "tmux -L arch -c 'tmux kill-pane -t %1'",
    ] {
        assert_eq!(evaluate_architect_pane(command, &probe), None, "{command}");
    }
}

/// #8902 LOW-1: a session renamed or created in the same command denies a hit
/// only on a server that holds an Architect pane.
#[test]
fn a_retarget_denies_only_on_a_server_holding_the_architect() {
    let probe = fake();
    let scratch = "tmux -L scratch new -d -s w \\; send-keys -t =w x";
    assert_eq!(evaluate_architect_pane(scratch, &probe), None);
    for command in [
        "tmux -L arch new -d -s w \\; send-keys -t =w x",
        "tmux new -d -s w \\; send-keys -t =w x",
    ] {
        let reason = evaluate_architect_pane(command, &probe);
        assert!(
            reason
                .as_deref()
                .is_some_and(|r| r.contains("renames or creates")),
            "{command}: {reason:?}"
        );
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
    let panes = parse_panes(text, &[300], &[]).expect("parse");
    let marks: Vec<(bool, bool)> = panes.iter().map(|p| (p.architect, p.marked)).collect();
    assert_eq!(marks, [(true, false), (false, true), (true, false)]);
    assert_eq!(panes[2].name, "w x");
    let by_name = parse_panes(text, &[], &["pm".into()]).expect("parse");
    let marks: Vec<bool> = by_name.iter().map(|p| p.architect).collect();
    assert_eq!(marks, [true, true, false]);
    assert!(parse_panes("%1\t@1\n", &[], &[]).is_err());
}

/// #8902 review: how a `list-panes` run maps to a pane list. No tmux and no
/// server are an empty list; any other failure is an error; an unreadable
/// lineage marks `tm-architect` and every recorded session, and with the
/// records unreadable too no listing is given (#8878 R1 round 2).
#[test]
fn a_pane_listing_run_is_classified() {
    type Lineage = Result<Vec<u32>, String>;
    type Sidecars = Result<Vec<String>, String>;
    type Marks = Result<Vec<bool>, String>;
    let text = "%1\t@1\t$1\t100\t0\ttm-architect\n%2\t@2\t$2\t300\t0\tpm\n";
    let custom = "%1\t@1\t$1\t100\t0\ttm-supervisor\n%2\t@2\t$2\t300\t0\tpm\n";
    let marks = |r: Result<Vec<Pane>, String>| -> Marks {
        r.map(|panes| panes.iter().map(|p| p.architect).collect())
    };
    let ran = |ok, stdout, stderr| Listed::Ran { ok, stdout, stderr };
    let unreadable: Lineage = Err("records unreadable".into());
    let none: Sidecars = Ok(vec![]);
    let supervisor: Sidecars = Ok(vec!["tm-supervisor".into()]);
    let stale_pm: Sidecars = Ok(vec!["pm".into()]);
    let no_sidecars: Sidecars = Err("sidecars unreadable".into());
    let neither = "the Architect launch records do not read (records unreadable; \
                   sidecars unreadable)";
    let cases: [(Listed<'_>, &Lineage, &Sidecars, Marks); 11] = [
        (Listed::NotFound, &Ok(vec![]), &none, Ok(vec![])),
        (
            Listed::Failed("spawn".into()),
            &Ok(vec![]),
            &none,
            Err("spawn".into()),
        ),
        (
            ran(false, "", "no server running on /tmp/tmux-501/default\n"),
            &Ok(vec![]),
            &none,
            Ok(vec![]),
        ),
        (
            ran(
                false,
                "",
                "error connecting to /tmp/tmux-501/x (No such file)",
            ),
            &Ok(vec![]),
            &none,
            Ok(vec![]),
        ),
        (
            ran(false, "", "server exited unexpectedly\n"),
            &Ok(vec![]),
            &none,
            Err("server exited unexpectedly".into()),
        ),
        (
            ran(true, text, ""),
            &Ok(vec![300]),
            &none,
            Ok(vec![true, true]),
        ),
        (
            ran(true, text, ""),
            &unreadable,
            &none,
            Ok(vec![true, false]),
        ),
        // #8878: a readable lineage never reads the sidecars, so a stale one
        // leaves a non-Architect session unmarked.
        (
            ran(true, text, ""),
            &Ok(vec![]),
            &stale_pm,
            Ok(vec![true, false]),
        ),
        (
            ran(true, custom, ""),
            &unreadable,
            &supervisor,
            Ok(vec![true, false]),
        ),
        (
            ran(true, custom, ""),
            &unreadable,
            &no_sidecars,
            Err(neither.into()),
        ),
        (
            ran(true, text, ""),
            &unreadable,
            &no_sidecars,
            Err(neither.into()),
        ),
    ];
    for (listed, lineage, sidecars, want) in cases {
        let found = architect_marks(lineage, || sidecars.clone());
        assert_eq!(marks(classify_listing(listed, &found)), want);
    }
}
