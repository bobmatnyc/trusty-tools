//! Unit tests for the #9001 exact-target floor (`tmux_exact_target.rs`).
//!
//! The fixtures spell non-exact tmux targets on purpose: each is a command the
//! classifier judges, built by [`aimed`], never a tmux call.

use super::super::architect_pane::{Pane, PaneProbe};
use super::super::architect_pane_probe::{Listed, LivePanes, list_panes};
use super::*;
use crate::test_support::tmux_session::{
    PrivateTmuxServer, ScratchTmuxSession, reserved_session_name,
};

/// A classifier fixture: `tmux <verb> -t <target>`.
fn aimed(verb: &str, target: &str) -> String {
    format!("tmux {verb} -t {target} hi")
}

fn row(session: (u8, &str), window: (u8, &str, &str, bool), pane: (u8, &str)) -> TmuxObject {
    TmuxObject {
        session: format!("${}", session.0),
        session_name: session.1.into(),
        window: format!("@{}", window.0),
        window_index: window.1.into(),
        window_name: window.2.into(),
        window_active: window.3,
        pane: format!("%{}", pane.0),
        pane_index: pane.1.into(),
    }
}

/// A server holding `pm` (`$2`: window 0 `main` with `%2`, window 1 `build`
/// with `%4` and `%5`) and `pm-long` (`$3`: window 0 `zsh` with `%3`); the
/// caller is `%2`.
struct Fake {
    objects: Result<Vec<TmuxObject>, String>,
}

fn fake() -> Fake {
    Fake {
        objects: Ok(vec![
            row((2, "pm"), (2, "0", "main", true), (2, "0")),
            row((2, "pm"), (4, "1", "build", false), (4, "0")),
            row((2, "pm"), (4, "1", "build", false), (5, "1")),
            row((3, "pm-long"), (3, "0", "zsh", true), (3, "0")),
        ]),
    }
}

impl PaneProbe for Fake {
    fn architect_live(&self) -> Result<bool, String> {
        Ok(false)
    }
    fn panes(&self, _server: &[String]) -> Result<Vec<Pane>, String> {
        Ok(Vec::new())
    }
    fn current_pane(&self) -> Option<String> {
        Some("%2".into())
    }
    fn objects(&self, _server: &[String]) -> Result<Vec<TmuxObject>, String> {
        self.objects.clone()
    }
}

/// A probe that keeps the trait's default listing.
struct NoListing;

impl PaneProbe for NoListing {
    fn architect_live(&self) -> Result<bool, String> {
        Ok(false)
    }
    fn panes(&self, _server: &[String]) -> Result<Vec<Pane>, String> {
        Ok(Vec::new())
    }
    fn current_pane(&self) -> Option<String> {
        None
    }
}

fn denied(probe: &dyn PaneProbe, command: &str) -> Option<String> {
    evaluate_tmux_exact_target(command, probe).filter(|r| r.contains("#9001"))
}

/// Owner ruling 2026-10-01: an exact target passes; a prefix-only, missing,
/// expanded or special target denies, naming the target.
#[test]
fn an_exact_target_passes_and_a_prefix_or_missing_one_denies() {
    let probe = fake();
    let exact = [
        "=pm:0",
        "pm:0",
        "pm:0.0",
        "pm:1.1",
        "=pm:build",
        "pm:=build.1",
        "pm:",
        ":1",
        "%3",
        "@4.1",
        "'$3'",
        "'$2:1'",
        "=pm-long",
        "pm-long",
        "pm",
        "=main",
    ];
    for target in exact {
        let command = aimed("send-keys", target);
        assert_eq!(denied(&probe, &command), None, "{command}");
    }
    let inexact = [
        "nosuch:0",
        "pm-:0",
        "pm-lo",
        "p",
        "ma",
        "pm:7",
        "pm:bui",
        "pm:0.9",
        "pm:0.x",
        "%99",
        "'$9'",
        "pm.1",
        "3",
        "pm:{last}",
        "\"$T\"",
        "=nosuch",
    ];
    for target in inexact {
        let command = aimed("send-keys", target);
        let reason = denied(&probe, &command).unwrap_or_else(|| panic!("{command} passed"));
        let shown = target.trim_matches(['\'', '"']);
        assert!(
            reason.contains(&format!("`{shown}`")),
            "{command}: {reason}"
        );
    }
    // Every deny verb and a source target are judged; read verbs are not.
    assert!(denied(&probe, &aimed("kill-session", "nosuch")).is_some());
    assert!(denied(&probe, "tmux swap-pane -s nosuch:0 -t =pm:0").is_some());
    assert_eq!(denied(&probe, &aimed("capture-pane -p", "nosuch:0")), None);
    assert_eq!(denied(&probe, &aimed("has-session", "nosuch")), None);
    assert_eq!(denied(&probe, "tmux send-keys hi"), None);
}

/// Fail closed: a server tmux will not list denies every target, whatever
/// the reason — no binary, a failed or refused spawn, a query error, a probe
/// with no listing — and no running server lists nothing, so its targets deny.
#[test]
fn an_unlistable_server_denies_every_target() {
    let command = aimed("send-keys", "=pm:0");
    let unlistable = Fake {
        objects: Err("tmux spawn refused".into()),
    };
    let reason = denied(&unlistable, &command).expect("an unlistable server denies");
    assert!(reason.contains("cannot list") && reason.contains("`=pm:0`"));
    assert!(
        denied(&NoListing, &command).is_some(),
        "the default listing"
    );
    let errors = [
        Listed::NotFound,
        Listed::Failed("permission denied".into()),
        Listed::Ran {
            ok: false,
            stdout: "",
            stderr: "lost server",
        },
        Listed::Ran {
            ok: true,
            stdout: "$1\t@1\t0\n",
            stderr: "",
        },
    ];
    for listed in errors {
        let shown = format!("{listed:?}");
        assert!(classify_objects(listed).is_err(), "{shown}");
    }
    // #9001 critic r2: a socket the guard may not open is no empty server.
    let unreadable = Fake {
        objects: classify_objects(Listed::Ran {
            ok: false,
            stdout: "",
            stderr: "error connecting to /tmp/tmux-1/default (Permission denied)",
        }),
    };
    assert!(unreadable.objects.is_err(), "{:?}", unreadable.objects);
    let reason = denied(&unreadable, &command).expect("an unreadable socket denies");
    assert!(reason.contains("cannot list"), "{reason}");
    let missing = Listed::Ran {
        ok: false,
        stdout: "",
        stderr: "error connecting to /tmp/tmux-1/x (No such file or directory)",
    };
    assert_eq!(classify_objects(missing), Ok(Vec::new()));
    let no_server = Listed::Ran {
        ok: false,
        stdout: "",
        stderr: "no server running on /tmp/tmux-1/x",
    };
    let empty = Fake {
        objects: classify_objects(no_server),
    };
    assert_eq!(empty.objects, Ok(Vec::new()));
    assert!(denied(&empty, &command).is_some(), "no server, no target");
}

/// The listing reads the eight fields of [`OBJECT_FORMAT`] per pane.
#[test]
fn a_listing_line_reads_into_one_object() {
    let listed = Listed::Ran {
        ok: true,
        stdout: "$2\t@4\t1\t0\t%5\t1\tpm\tbuild\n",
        stderr: "",
    };
    let want = row((2, "pm"), (4, "1", "build", false), (5, "1"));
    assert_eq!(classify_objects(listed), Ok(vec![want]));
}

/// Owner ruling 2026-10-01, on a real private tmux server (`-L`): a session
/// named `<X>-<pid>` exists and `<X>` does not. A target naming `<X>` is a
/// prefix-only match tmux would send to the live session, so it denies; the
/// exact name passes. Skips where tmux is not installed.
#[serial_test::serial]
#[test]
fn a_live_prefix_collision_denies_on_a_private_server() {
    const TMUX: &str = "tmux";
    if !ScratchTmuxSession::tmux_available(TMUX) {
        eprintln!("tmux not available; skipping");
        return;
    }
    let server = PrivateTmuxServer::new(TMUX, "9001");
    let live = reserved_session_name("9001collide");
    let _session = ScratchTmuxSession::spawn_on_socket(TMUX, Some(server.name()), &live, "sh");
    let (prefix, _) = live
        .rsplit_once('-')
        .expect("a reserved name carries its pid");
    let probe = LivePanes::ambient();
    let on_server = |target: &str| aimed(&format!("-L {} send-keys", server.name()), target);
    for target in [
        format!("={live}"),
        format!("{live}:"),
        format!("={live}:.0"),
    ] {
        let command = on_server(&target);
        assert_eq!(denied(&probe, &command), None, "{command}");
    }
    for target in [
        format!("{prefix}:"),
        prefix.to_owned(),
        "nosuch9001:0".into(),
    ] {
        let command = on_server(&target);
        let reason = denied(&probe, &command).unwrap_or_else(|| panic!("{command} passed"));
        assert!(reason.contains(&format!("`{target}`")), "{reason}");
    }
}

/// #9001 critic r1, finding 1: a tmux command the guard cannot read denies
/// with no Architect live (the fakes report none), naming its token or
/// reason — one row per opaque class.
#[test]
fn every_unreadable_tmux_command_denies_naming_why() {
    let probe = fake();
    let send = aimed("send-keys", "=pm:0");
    let rows: [(String, &str); 9] = [
        (format!("TMUX_TMPDIR=/tmp/x9001 {send}"), "`TMUX`"),
        (format!("env -i {send}"), "`env -i`"),
        (format!("env -i PATH=/usr/bin {send}"), "`env -i`"),
        (format!("sudo {send}"), "`sudo`"),
        (format!("exec -c {send}"), "`exec -c`"),
        (
            send.replacen("tmux", "tmux -S ./sock9001", 1),
            "relative `-S`",
        ),
        (send.replacen("tmux", "T=tmux; $T", 1), "`$T`"),
        (aimed("send-keys -Z", "=pm:0"), "does not know"),
        (format!("{send} 'unclosed"), "does not parse"),
    ];
    for (command, named) in rows {
        let reason = denied(&probe, &command).unwrap_or_else(|| panic!("{command} passed"));
        assert!(reason.contains("cannot read"), "{command}: {reason}");
        assert!(reason.contains(named), "{command}: {reason}");
    }
    // An unparseable command with no tmux is not this floor's (#9001 case 1).
    assert_eq!(denied(&probe, r"grep -c $'\u001b' log"), None);
}

/// #9001 critic r1, finding 2: a tmux command after shell grammar — a group,
/// a subshell, a compound command, a function body — is still judged.
#[test]
fn a_tmux_command_after_shell_grammar_is_judged() {
    let probe = fake();
    let send = aimed("send-keys", "nosuch");
    let has = aimed("has-session", "=pm").replace(" hi", "");
    for command in [
        format!("{{ {send}; }}"),
        format!("( {send} )"),
        format!("({send})"),
        format!("! {send}"),
        format!("if true; then {send}; fi"),
        format!("if false; then :; else {send}; fi"),
        format!("if false; then :; elif true; then {send}; fi"),
        format!("for x in a; do {send}; done"),
        format!("while true; do {send}; done"),
        format!("until false; do {send}; done"),
        format!("{has}; if {has}; then {send}; fi"),
        format!("f() {{ {send}; }}; f"),
        format!("f () {{ {send}; }}; f"),
        format!("function f {{ {send}; }}; f"),
        format!("case x in x) {send};; esac"),
        // #9001 critic r2.
        format!("f(){{ {send}; }}; f"),
        format!("f (){{ {send}; }}; f"),
        format!("time {{ {send}; }}"),
        format!("time ({send})"),
        format!("coproc {send}"),
        format!("coproc NAME {{ {send}; }}"),
        format!("case x in (x) {send};; esac"),
    ] {
        let reason = denied(&probe, &command).unwrap_or_else(|| panic!("{command} passed"));
        assert!(reason.contains("`nosuch`"), "{command}: {reason}");
    }
    assert_eq!(denied(&probe, &format!("if {has}; then echo up; fi")), None);
}

/// #9001 critic r2: the bypasses its probe found ALLOWED, each now denied.
/// The grammar rows are judged, so their `nos` target is named; the rest are
/// unreadable, so the guard says it cannot read them.
const R2_BYPASSES: [(&str, &str); 13] = [
    ("T=tm''ux; $T send-keys -t nos hi", "cannot read"),
    ("T=tm; ${T}ux send-keys -t nos hi", "cannot read"),
    ("a=t b=mux; $a$b send-keys -t nos hi", "cannot read"),
    ("set -- -t nos; tmux send-keys \"$@\" hi", "cannot read"),
    ("F=; tmux send-keys $F -t nos hi", "cannot read"),
    ("A='-t nos'; tmux send-keys $A hi", "cannot read"),
    ("echo '-t nos hi' | xargs tmux send-keys", "cannot read"),
    ("f(){ tmux send-keys -t nos hi; }; f", "`nos`"),
    ("time { tmux send-keys -t nos hi; }", "`nos`"),
    ("coproc tmux send-keys -t nos hi", "`nos`"),
    ("case x in (x) tmux send-keys -t nos hi;; esac", "`nos`"),
    ("bash <<< 'tmux send-keys -t nos hi'", "cannot read"),
    ("echo 'tmux send-keys -t nos hi' | sh", "cannot read"),
];

/// Assert each `(command, named)` row denies under #9001, naming `named`.
fn assert_rows_deny(probe: &dyn PaneProbe, rows: &[(&str, &str)]) {
    for (command, named) in rows {
        let reason = denied(probe, command).unwrap_or_else(|| panic!("{command} passed"));
        assert!(reason.contains(named), "{command}: {reason}");
    }
}

/// #9001 critic r2, rule (a): a program name the shell expands, in a segment
/// that reads as tmux, is unreadable whatever its literal spelling.
#[test]
fn a_program_name_the_shell_expands_denies() {
    assert_rows_deny(&fake(), &R2_BYPASSES[..3]);
    assert_rows_deny(
        &fake(),
        &[
            ("$(printf 't%sux' m) kill-server", "cannot read"),
            ("X=x; $X -L s9001 send-keys -t =pm:0 hi", "cannot read"),
            ("$T send-keys -t nos hi", "`$T`"),
            ("$T kill-session -t x", "`$T`"),
            ("$T killp -t =pm:0", "`$T`"),
            ("$T send-k -t =pm:0 hi", "`$T`"),
            ("Tmux send-keys -t nos hi", "`nos`"),
        ],
    );
    // Supervisor ruling 2026-10-02, narrow reading: with no tmux text and no
    // deny verb in a verb position, a dynamic program word passes. `$P "$A"`
    // is the accepted residual.
    for command in ["$EDITOR \"$FILE\"", "$P \"$A\"", "X=x; $X $Y -t =pm:0"] {
        assert_eq!(denied(&fake(), command), None, "{command}");
    }
}

/// #9001 critic r2, rule (a): a word the shell expands before `--` ends the
/// options may expand to `-t <target>`, so it is unreadable.
#[test]
fn a_word_the_shell_expands_before_the_options_end_denies() {
    assert_rows_deny(&fake(), &R2_BYPASSES[3..6]);
    let message = "tmux send-keys -t =pm:0 \"$MSG\"";
    assert_rows_deny(&fake(), &[(message, "before `--`")]);
    let after = "tmux send-keys -t =pm:0 -- \"$MSG\"";
    assert_eq!(denied(&fake(), after), None, "{after}");
}

/// #9001 critic r2, rule (b): tmux behind `xargs` or `find -exec … +`, and a
/// shell that reads tmux program text on stdin, are opaque.
#[test]
fn every_opaque_or_dynamic_tmux_route_denies() {
    assert_rows_deny(&fake(), &R2_BYPASSES[6..7]);
    assert_rows_deny(&fake(), &R2_BYPASSES[11..]);
    assert_rows_deny(
        &fake(),
        &[
            ("echo nos | xargs -n1 tmux send-keys -t =pm:0", "`xargs`"),
            ("find . -exec tmux send-keys -t =pm:0 {} +", "`find -exec"),
            ("echo 'tmux send-keys -t nos hi' | bash -s", "stdin"),
            ("tmux ls; sh < /tmp/x9001.sh", "stdin"),
        ],
    );
    // A literal `find -exec … \;` is read; a fed shell with inline code is
    // the wrapper's to judge; a fed shell in a command naming no tmux passes.
    for command in [
        "find . -maxdepth 0 -exec tmux send-keys -t =pm:0 hi \\;",
        "echo hi | bash -c 'tmux ls'",
        "echo 'make test' | sh",
    ] {
        assert_eq!(denied(&fake(), command), None, "{command}");
    }
}

/// #9001 critic r3: the bypasses its review found ALLOWED at 9d47158a85.
const R3_BYPASSES: [(&str, &str); 6] = [
    ("echo tmux | xargs -I{} env {} kill-server", "`xargs`"),
    ("echo tmux | xargs -J % % kill-server", "`xargs`"),
    ("echo tmux | xargs --replace={} {} kill-server", "`xargs`"),
    (
        "read -d '' C <<EOF\ntmux kill-server\nEOF\neval \"$C\"",
        "cannot read",
    ),
    (
        "while read a b; do $a $b; done <<EOF\ntmux kill-server\nEOF",
        "cannot read",
    ),
    ("bash <<< 'TMUX killp'", "stdin"),
];

/// #9001 critic r3, findings 1, 3 and 4: an `xargs` replacement string can
/// make a stdin word the program, a shell fed `TMUX` text runs tmux, and a
/// shell given a script operand reads its stdin as data.
#[test]
fn an_xargs_replacement_string_or_a_fed_shell_naming_tmux_denies() {
    assert_rows_deny(&fake(), &R3_BYPASSES[..3]);
    assert_rows_deny(&fake(), &R3_BYPASSES[5..]);
    assert_rows_deny(
        &fake(),
        &[
            ("echo tmux | xargs -I {} {} kill-server", "`xargs`"),
            ("echo tmux | xargs -0i{} {} kill-server", "`xargs`"),
            ("echo tmux | xargs -n 1 -I{} {} kill-server", "`xargs`"),
            ("echo 'Tmux kill-server' | sh", "stdin"),
            ("cat x | bash -s tmux", "stdin"),
            ("echo 'tmux kill-server' | bash -o pipefail", "stdin"),
            ("tmux ls; python3 - < /tmp/x9001.py", "stdin"),
        ],
    );
    for command in [
        "cat log | bash scripts/report.sh tmux",
        "cat log | bash -o pipefail scripts/report.sh tmux",
        "tmux ls | python3 -m json.tool",
        "tmux ls | python3 scripts/summarize.py",
        "tmux ls | xargs -n1 echo",
        "echo a | xargs -I{} echo {}",
    ] {
        assert_eq!(denied(&fake(), command), None, "{command}");
    }
}

/// #9001 critic r3, finding 2: a here-document body read into a variable an
/// evaluator runs, or fed to a loop that runs its words, is read as shell.
/// A data body in a command with neither still passes.
#[test]
fn a_data_body_an_evaluator_or_dynamic_program_runs_is_read() {
    assert_rows_deny(&fake(), &R3_BYPASSES[3..5]);
    assert_rows_deny(
        &fake(),
        &[
            (
                "read -d '' C <<EOF; eval \"$C\"\ntmux kill-server\nEOF",
                "cannot read",
            ),
            (
                "read -r -d '' C <<'EOF'\ntmux send-keys -t nos hi\nEOF\n$C",
                "cannot read",
            ),
        ],
    );
    for command in [
        "cat <<'EOF' > notes.md\nThe PM's tmux pane is fine\nEOF\ngit add notes.md",
        "while read a; do echo \"$a\"; done <<EOF\ntmux kill-server\nEOF",
    ] {
        assert_eq!(denied(&fake(), command), None, "{command}");
    }
}

/// Owner ruling 2026-10-01 (ruling 353): `send-keys` to a target that does
/// not exist is refused, naming it — on a server that lists other sessions
/// and on one with no server running.
#[test]
fn send_keys_to_a_missing_target_is_refused() {
    let empty = Fake {
        objects: Ok(Vec::new()),
    };
    let listed = fake();
    for probe in [&listed as &dyn PaneProbe, &empty] {
        for target in ["nosuch:0", "nosuch", "=nosuch:0", "nosuch:0.0", "%99"] {
            let command = format!("tmux send-keys -t {target} 'hi' Enter");
            let reason = denied(probe, &command).unwrap_or_else(|| panic!("{command} passed"));
            assert!(reason.contains(&format!("`{target}`")), "{reason}");
        }
    }
}

/// #9001 critic r2, rules (c) and (d): every grammar row is judged and every
/// over-denial passes — a literal program path, prose in a here-document
/// data body, and the read verbs the P10 exception relies on.
#[test]
fn prose_and_a_literal_program_path_are_not_refused() {
    assert_rows_deny(&fake(), &R2_BYPASSES[7..11]);
    for command in [
        "~/.cargo/bin/tm doctor | grep tmux",
        "cat <<'EOF' > notes.md\nThe PM's tmux pane is fine\nEOF",
        "gh issue comment 1 --body \"$(cat <<'EOF'\nWe can't trust tmux send-keys here\nEOF\n)\"",
        "\"$CARGO_TARGET_DIR/debug/tm\" fleet status --tmux",
        "$EDITOR notes.md",
        "tmux capture-pane -t =pm:0 -p",
        "tmux has-session -t nos",
        "tmux ls",
        "tmux display-message -p '#{session_name}'",
        "tmux list-panes -a",
    ] {
        assert_eq!(denied(&fake(), command), None, "{command}");
    }
    // A body a shell runs, or one that expands a substitution, is still read.
    for command in [
        "bash <<'EOF'\ntmux send-keys -t nos hi\nEOF",
        "cat <<EOF > notes.md\n$(tmux send-keys -t nos hi)\nEOF",
    ] {
        assert!(denied(&fake(), command).is_some(), "{command}");
    }
}

/// #9001 critic r1, finding 3: `kill-session -a` still names its `-t`.
#[test]
fn kill_session_all_still_judges_its_target() {
    let probe = fake();
    let reason = denied(&probe, &aimed("kill-session -a", "nosuch")).expect("denied");
    assert!(reason.contains("`nosuch`"), "{reason}");
    assert_eq!(denied(&probe, &aimed("kill-session -a", "=pm")), None);
}

/// #9001 critic r1, finding 4: tmux reads a pane-position word as a pane of
/// the caller's window before any session, so it is no exact pane target,
/// in any case. A session verb, and `=name`, still reach the session.
#[test]
fn a_pane_position_word_is_no_exact_pane_target() {
    let mut objects = fake().objects.expect("fake rows");
    for (n, name) in [(6, "top"), (7, "Bottom-Left"), (8, "right")] {
        objects.push(row((n, name), (n, "0", "zsh", true), (n, "0")));
    }
    let probe = Fake {
        objects: Ok(objects),
    };
    for target in ["top", "Bottom-Left", "RIGHT"] {
        for verb in ["send-keys", "kill-pane", "respawn-pane"] {
            let command = aimed(verb, target);
            let reason = denied(&probe, &command).unwrap_or_else(|| panic!("{command} passed"));
            assert!(reason.contains("pane position"), "{command}: {reason}");
        }
    }
    for command in [
        aimed("kill-session", "top"),
        aimed("send-keys", "=top"),
        aimed("send-keys", "top:0"),
    ] {
        assert_eq!(denied(&probe, &command), None, "{command}");
    }
}

/// #9001 critic r1, finding 5: a listing that times out is an `Err`, so its
/// targets deny.
#[test]
fn a_timed_out_listing_is_an_error() {
    let listing = list_panes(&[], OBJECT_FORMAT, classify_objects, |_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "tmux did not answer",
        ))
    });
    let err = listing.expect_err("a timeout is no listing");
    assert!(err.contains("did not answer"), "{err}");
}

/// SIGCONTs a stopped tmux server on drop, so a failed test leaves no
/// stopped server behind.
struct Resume(libc::pid_t);

impl Drop for Resume {
    fn drop(&mut self) {
        // SAFETY: a signal to the private server this test started.
        unsafe {
            libc::kill(self.0, libc::SIGCONT);
        }
    }
}

/// #9001 critic r1, finding 5, on a real private tmux server (`-L`) that is
/// stopped (SIGSTOP): the listing gives up inside its budget and is an `Err`,
/// and a later listing in the same probe fails at once. Skips where tmux is
/// not installed.
#[serial_test::serial]
#[test]
fn a_stopped_tmux_server_times_out_the_listing() {
    const TMUX: &str = "tmux";
    if !ScratchTmuxSession::tmux_available(TMUX) {
        eprintln!("tmux not available; skipping");
        return;
    }
    let server = PrivateTmuxServer::new(TMUX, "9001stop");
    let name = reserved_session_name("9001stop");
    let _session = ScratchTmuxSession::spawn_on_socket(TMUX, Some(server.name()), &name, "sh");
    let pid: libc::pid_t = server
        .query(&["display-message", "-p", "#{pid}"])
        .and_then(|p| p.parse().ok())
        .expect("the private server's pid");
    // SAFETY: stops the private server this test started; `Resume` restarts it.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
    let _resume = Resume(pid);
    let probe = LivePanes::ambient();
    let argv = ["-L".to_owned(), server.name().to_owned()];
    let started = std::time::Instant::now();
    let listing = probe.objects(&argv);
    let first = started.elapsed();
    let err = listing.expect_err("a stopped server lists nothing");
    assert!(err.contains("did not answer"), "{err}");
    assert!(first < std::time::Duration::from_secs(4), "took {first:?}");
    let again = std::time::Instant::now();
    assert!(probe.panes(&argv).is_err());
    assert!(again.elapsed() < std::time::Duration::from_millis(500));
}

/// 🔴 REGRESSION (#9053, case 1): a runner that runs its operands — `watch`,
/// BSD `script` after its file, util-linux `script -c` — reaches tmux. Each
/// row passed at 156a72be84. A runner of a read-only tmux call or of another
/// program still passes.
#[test]
fn a_runner_running_tmux_is_read_9053() {
    assert_rows_deny(
        &fake(),
        &[
            ("watch tmux send-keys -t nos hi", "`nos`"),
            ("watch -n 1 tmux send-keys -t nos hi", "`nos`"),
            ("watch 'tmux send-keys -t nos hi'", "`nos`"),
            ("script -q /dev/null tmux send-keys -t nos hi", "`nos`"),
            ("script -c 'tmux send-keys -t nos hi' /dev/null", "`nos`"),
        ],
    );
    for command in [
        "watch -n 5 tmux ls",
        "watch -n 5 make test",
        "script -q out.log make",
    ] {
        assert_eq!(denied(&fake(), command), None, "{command}");
    }
}

/// 🔴 REGRESSION (#9053, cases 1-2): `xargs` into a wrapper with no program
/// of its own takes the program from stdin, and GNU `parallel` builds its
/// commands from inputs; with tmux named, each is unreadable. Each row
/// passed at 156a72be84. Neither route naming no tmux is refused.
#[test]
fn xargs_into_a_wrapper_or_runner_naming_tmux_denies_9053() {
    assert_rows_deny(
        &fake(),
        &[
            ("echo 'tmux send-keys -t nos hi' | xargs env", "`xargs`"),
            ("echo 'tmux send-keys -t nos hi' | xargs sudo", "`xargs`"),
            ("echo 'tmux send-keys -t nos hi' | xargs nice", "`xargs`"),
            (
                "echo 'tmux send-keys -t nos hi' | xargs timeout 5",
                "`xargs`",
            ),
            (
                "echo 'tmux send-keys -t nos hi' | xargs -n 2 env",
                "`xargs`",
            ),
            ("echo 'tmux send-keys -t nos hi' | xargs watch", "`xargs`"),
            ("parallel ::: 'tmux send-keys -t nos hi'", "`parallel`"),
            ("echo send-keys | parallel tmux", "`parallel`"),
        ],
    );
    for command in [
        "echo a b | xargs env",
        "ls | parallel gzip",
        "echo x | xargs nice echo",
    ] {
        assert_eq!(denied(&fake(), command), None, "{command}");
    }
}

/// 🔴 REGRESSION (#9053, case 3): unparseable text naming `TMUX` in any case
/// may run tmux (APFS is case-insensitive). The row passed at 156a72be84.
#[test]
fn unparseable_text_naming_tmux_in_any_case_denies_9053() {
    use super::super::architect_pane_parse::may_run_tmux;
    assert!(may_run_tmux("TMUX kill-server 'x", 1));
    assert!(may_run_tmux("Tmux kill-server 'x", 1));
    assert!(!may_run_tmux("TMUXED kill-server 'x", 1));
    assert_rows_deny(
        &fake(),
        &[("echo \"$(TMUX send-keys -t nos hi 'x)\"", "cannot read")],
    );
}

/// 🔴 REGRESSION (#9053, case 4): an evaluator option the guard does not
/// know may take the next word as its value, so that word is not read as a
/// script and the fed shell still denies. Each row passed at 156a72be84; a
/// known flag before a script still passes.
#[test]
fn an_unknown_evaluator_option_is_no_script_operand_9053() {
    assert_rows_deny(
        &fake(),
        &[
            (
                "echo 'tmux send-keys -t nos hi' | node --stack-size 100",
                "stdin",
            ),
            ("echo 'tmux send-keys -t nos hi' | ruby -E utf-8", "stdin"),
            (
                "echo 'tmux send-keys -t nos hi' | python3 --check-hash-based-pycs always",
                "stdin",
            ),
        ],
    );
    for command in [
        "cat log | python3 -u scripts/summarize.py tmux",
        "cat log | bash -x scripts/report.sh tmux",
        "cat log | ruby -E utf-8 report.rb tmux",
    ] {
        assert_eq!(denied(&fake(), command), None, "{command}");
    }
}
