//! Unit tests for the #9001 exact-target floor (`tmux_exact_target.rs`).
//!
//! The fixtures spell non-exact tmux targets on purpose: each is a command the
//! classifier judges, built by [`aimed`], never a tmux call.

use super::super::architect_pane::{Pane, PaneProbe};
use super::super::architect_pane_probe::{Listed, LivePanes};
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
