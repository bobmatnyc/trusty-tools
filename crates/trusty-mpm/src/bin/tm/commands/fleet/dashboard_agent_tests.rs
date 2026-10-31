//! Tests for the dashboard's LaunchAgent install, guard and takeover (#10571 P1b).
//! A fake [`Host`] stands in for lsof, launchctl, kill and HTTP, so the
//! decisions run on any platform; the plist render is checked on macOS.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use super::super::Step;
use super::super::dashboard_agent::{self as agent, Existing, Host, Listener, SCRIPT};
use super::{Fixture, seeded_files};

/// What the fake host reports, and what it was asked to do.
#[derive(Default)]
struct Fake {
    listener: RefCell<Option<Listener>>,
    loaded: RefCell<bool>,
    /// Whether a freshly loaded agent answers.
    agent_answers: bool,
    /// Whether the old process answers again after a respawn.
    respawn_answers: bool,
    stop_fails: bool,
    render_fails: bool,
    bootstrap_fails: bool,
    /// The listener probe cannot tell (lsof missing or failing).
    probe_fails: bool,
    /// A different process answers the re-check before the stop.
    swapped: RefCell<Option<Listener>>,
    calls: RefCell<Vec<String>>,
}

impl Fake {
    fn log(&self, s: impl Into<String>) {
        self.calls.borrow_mut().push(s.into());
    }
    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Host for Fake {
    fn listener(&self, _port: u16) -> Result<Option<Listener>, String> {
        if self.probe_fails {
            return Err("lsof failed".to_owned());
        }
        if let Some(other) = self.swapped.borrow_mut().take() {
            // the first read classifies; the re-check sees another process
            let first = self.listener.borrow().clone();
            *self.listener.borrow_mut() = Some(other);
            return Ok(first);
        }
        Ok(self.listener.borrow().clone())
    }
    fn agent_loaded(&self, _label: &str) -> bool {
        *self.loaded.borrow()
    }
    fn render(&self, dir: &Path, port: u16) -> anyhow::Result<String> {
        anyhow::ensure!(!self.render_fails, "render failed");
        Ok(format!("<plist dir={} port={port}/>", dir.display()))
    }
    fn bootstrap(&self, _plist: &Path) -> anyhow::Result<()> {
        self.log("bootstrap");
        anyhow::ensure!(!self.bootstrap_fails, "launchctl bootstrap failed");
        *self.loaded.borrow_mut() = true;
        Ok(())
    }
    fn bootout(&self, _label: &str) -> anyhow::Result<()> {
        self.log("bootout");
        *self.loaded.borrow_mut() = false;
        Ok(())
    }
    fn terminate(&self, pid: u32) -> bool {
        self.log(format!("terminate {pid}"));
        if self.stop_fails {
            return false;
        }
        *self.listener.borrow_mut() = None;
        true
    }
    fn respawn(&self, dir: &Path, port: u16) -> anyhow::Result<()> {
        self.log(format!("respawn {port}"));
        *self.listener.borrow_mut() = Some(known(dir));
        Ok(())
    }
    fn serving(&self, _port: u16) -> bool {
        if *self.loaded.borrow() {
            self.agent_answers
        } else {
            self.listener.borrow().is_some() && self.respawn_answers
        }
    }
}

fn shipped() -> &'static str {
    seeded_files()
        .into_iter()
        .find(|f| f.dest == SCRIPT)
        .unwrap()
        .contents
}

/// The takeover case: the known dashboard, pid 4242, from the Architect directory on :7890.
fn known(dir: &Path) -> Listener {
    Listener {
        pid: 4242,
        argv: vec![
            "python3".into(),
            SCRIPT.into(),
            "--port".into(),
            "7890".into(),
        ],
        cwd: Some(dir.to_path_buf()),
    }
}

fn setup() -> (Fixture, PathBuf, PathBuf) {
    let fx = Fixture::new();
    fx.init().unwrap();
    let dir = fx.dir();
    let home = fx.home().to_path_buf();
    (fx, dir, home)
}

#[test]
fn a_free_port_installs_and_loads_the_agent() {
    let (_fx, dir, home) = setup();
    let host = Fake {
        agent_answers: true,
        ..Fake::default()
    };
    let steps = agent::steps(&dir, &home, true, false, &host, shipped());
    assert!(
        matches!(&steps[0], Step::Changed(t) if t.contains("start at login")),
        "{steps:?}"
    );
    let plist = std::fs::read_to_string(agent::plist_path(&home)).unwrap();
    assert!(plist.contains("port=7890"));
    assert_eq!(host.calls(), vec!["bootstrap"]);
}

#[test]
fn a_failed_install_is_undone() {
    let (_fx, dir, home) = setup();
    let host = Fake {
        agent_answers: false,
        ..Fake::default()
    };
    let steps = agent::steps(&dir, &home, true, false, &host, shipped());
    assert!(
        matches!(&steps[0], Step::Failed(t) if t.contains("undone")),
        "{steps:?}"
    );
    assert!(!agent::plist_path(&home).exists());
    assert_eq!(host.calls(), vec!["bootstrap", "bootout"]);
}

#[test]
fn a_running_agent_is_left_alone_and_no_second_copy_starts() {
    let (_fx, dir, home) = setup();
    let host = Fake {
        agent_answers: true,
        ..Fake::default()
    };
    *host.loaded.borrow_mut() = true;
    let steps = agent::steps(&dir, &home, true, true, &host, shipped());
    assert!(matches!(&steps[0], Step::Unchanged(_)), "{steps:?}");
    assert!(host.calls().is_empty(), "{:?}", host.calls());
}

#[test]
fn a_known_dashboard_is_classified_known() {
    let (_fx, dir, _home) = setup();
    let host = Fake::default();
    *host.listener.borrow_mut() = Some(known(&dir));
    assert_eq!(
        agent::classify(&dir, 7890, &host, shipped()),
        Existing::KnownManaged(known(&dir))
    );
}

#[test]
fn a_known_dashboard_without_the_flag_is_reported_not_touched() {
    let (_fx, dir, home) = setup();
    let host = Fake::default();
    *host.listener.borrow_mut() = Some(known(&dir));
    let steps = agent::steps(&dir, &home, true, false, &host, shipped());
    assert!(
        matches!(&steps[0], Step::Skipped(t) if t.contains("--dashboard-takeover") && t.contains("Nothing was changed")),
        "{steps:?}"
    );
    assert!(host.calls().is_empty());
}

#[test]
fn a_known_dashboard_is_taken_over() {
    let (_fx, dir, home) = setup();
    let host = Fake {
        agent_answers: true,
        ..Fake::default()
    };
    *host.listener.borrow_mut() = Some(known(&dir));
    let steps = agent::steps(&dir, &home, true, true, &host, shipped());
    assert!(
        matches!(&steps[0], Step::Changed(t) if t.contains("stopped pid 4242")),
        "{steps:?}"
    );
    assert_eq!(host.calls(), vec!["terminate 4242", "bootstrap"]);
    assert!(agent::plist_path(&home).is_file());
}

#[test]
fn a_failed_takeover_puts_the_old_dashboard_back() {
    let (_fx, dir, home) = setup();
    let host = Fake {
        agent_answers: false,
        respawn_answers: true,
        ..Fake::default()
    };
    *host.listener.borrow_mut() = Some(known(&dir));
    let steps = agent::steps(&dir, &home, true, true, &host, shipped());
    assert!(
        matches!(&steps[0], Step::Failed(t) if t.contains("started again and is answering")),
        "{steps:?}"
    );
    assert_eq!(
        host.calls(),
        vec!["terminate 4242", "bootstrap", "bootout", "respawn 7890"]
    );
    assert!(!agent::plist_path(&home).exists(), "no plist left behind");
}

#[test]
fn a_dashboard_that_will_not_stop_is_left_running() {
    let (_fx, dir, home) = setup();
    let host = Fake {
        stop_fails: true,
        ..Fake::default()
    };
    *host.listener.borrow_mut() = Some(known(&dir));
    let steps = agent::steps(&dir, &home, true, true, &host, shipped());
    assert!(
        matches!(&steps[0], Step::Failed(t) if t.contains("did not exit within 10 seconds")),
        "{steps:?}"
    );
    assert_eq!(host.calls(), vec!["terminate 4242"]);
}

#[test]
fn an_unknown_listener_is_refused() {
    let (_fx, dir, home) = setup();
    let other = PathBuf::from("/srv/elsewhere");
    for (l, why) in [
        (
            Listener {
                pid: 7,
                argv: vec!["nginx".into()],
                cwd: Some(dir.clone()),
            },
            "not a dashboard-serve.py process",
        ),
        (
            Listener {
                pid: 7,
                argv: vec!["python3".into(), SCRIPT.into()],
                cwd: Some(other),
            },
            "another folder",
        ),
    ] {
        let host = Fake::default();
        *host.listener.borrow_mut() = Some(l);
        let steps = agent::steps(&dir, &home, true, true, &host, shipped());
        assert!(
            matches!(&steps[0], Step::Skipped(t) if t.contains(why) && t.contains("nothing was stopped")),
            "{steps:?}"
        );
        assert!(host.calls().is_empty(), "{:?}", host.calls());
    }
    // A hand-edited script is not ours to replace either.
    std::fs::write(dir.join(SCRIPT), "print('mine')\n").unwrap();
    let host = Fake::default();
    *host.listener.borrow_mut() = Some(known(&dir));
    assert!(matches!(
        agent::classify(&dir, 7890, &host, shipped()),
        Existing::Unknown(_, why) if why.contains("changed by hand")
    ));
}

#[test]
fn a_restart_keeps_edited_state_and_starts_no_duplicate() {
    let (fx, dir, home) = setup();
    let host = Fake {
        agent_answers: true,
        ..Fake::default()
    };
    agent::steps(&dir, &home, true, false, &host, shipped());
    // The owner edits a seeded file and the store gains a document.
    std::fs::write(dir.join("scripts/stale-tasks.py"), "print('mine')\n").unwrap();
    let doc = dir.join("local/owner-queue/items/D01.json");
    std::fs::create_dir_all(doc.parent().unwrap()).unwrap();
    std::fs::write(&doc, "{\"kind\":\"question\"}\n").unwrap();
    // After a login (launchd started it again), init runs once more.
    let report = fx.init().unwrap();
    let steps = agent::steps(&dir, &home, true, true, &host, shipped());
    assert_eq!(
        std::fs::read_to_string(dir.join("scripts/stale-tasks.py")).unwrap(),
        "print('mine')\n"
    );
    assert_eq!(
        std::fs::read_to_string(&doc).unwrap(),
        "{\"kind\":\"question\"}\n"
    );
    assert!(!report.failed(), "{}", report.render());
    assert!(matches!(&steps[0], Step::Unchanged(_)), "{steps:?}");
    assert_eq!(
        host.calls(),
        vec!["bootstrap"],
        "one load, never a second copy"
    );
}

#[test]
fn no_launch_changes_nothing() {
    let (_fx, dir, home) = setup();
    let host = Fake::default();
    let steps = agent::steps(&dir, &home, false, true, &host, shipped());
    assert!(matches!(&steps[0], Step::Skipped(_)));
    assert!(host.calls().is_empty() && !agent::plist_path(&home).exists());
}

#[test]
fn the_port_comes_from_local_architect_json() {
    let (_fx, dir, _home) = setup();
    assert_eq!(agent::port_of(&dir), 7890);
    std::fs::write(
        dir.join("local/architect.json"),
        "{\"dashboard_port\": 7999}",
    )
    .unwrap();
    assert_eq!(agent::port_of(&dir), 7999);
}

#[cfg(target_os = "macos")]
#[test]
fn the_plist_has_the_login_and_restart_keys() {
    let dir = Path::new("/Users/example/trusty-mpm-projects/architect");
    let xml = agent::render_plist(dir, 7890).unwrap();
    for key in [
        "<key>RunAtLoad</key>",
        "<key>KeepAlive</key>",
        "<key>SuccessfulExit</key>",
        "<key>WorkingDirectory</key>",
    ] {
        assert!(xml.contains(key), "missing {key}:\n{xml}");
    }
    assert!(
        xml.contains("/Users/example/trusty-mpm-projects/architect/scripts/dashboard-serve.py")
    );
    assert!(xml.contains("<string>7890</string>"));
    assert!(xml.contains(&agent::label()));
    assert!(xml.contains("inbox/dashboard-logs/stdout.log"));
}

/// The plist bytes a pre-existing install left, for the survival cases.
const ORIGINAL: &str = "<plist>the owner's earlier agent</plist>\n";

#[test]
fn a_pre_existing_plist_survives_every_failure() {
    for case in ["render", "bootstrap", "verify", "takeover"] {
        let (_fx, dir, home) = setup();
        let plist = agent::plist_path(&home);
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, ORIGINAL).unwrap();
        let host = Fake {
            render_fails: case == "render",
            bootstrap_fails: case == "bootstrap",
            agent_answers: false,
            respawn_answers: true,
            ..Fake::default()
        };
        if case == "takeover" {
            *host.listener.borrow_mut() = Some(known(&dir));
        }
        let steps = agent::steps(&dir, &home, true, true, &host, shipped());
        assert!(matches!(&steps[0], Step::Failed(_)), "{case}: {steps:?}");
        assert_eq!(
            std::fs::read_to_string(&plist).unwrap(),
            ORIGINAL,
            "{case}: plist changed"
        );
        assert!(!plist.with_extension("plist.bak").exists(), "{case}");
        let trace = host.calls();
        match case {
            "render" => assert!(trace.is_empty(), "{trace:?}"),
            "bootstrap" | "verify" => assert_eq!(trace, vec!["bootstrap", "bootout"]),
            _ => assert_eq!(
                trace,
                vec!["terminate 4242", "bootstrap", "bootout", "respawn 7890"]
            ),
        }
    }
}

#[test]
fn a_replaced_plist_is_kept_as_bak() {
    let (_fx, dir, home) = setup();
    let plist = agent::plist_path(&home);
    std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
    std::fs::write(&plist, ORIGINAL).unwrap();
    let host = Fake {
        agent_answers: true,
        ..Fake::default()
    };
    let steps = agent::steps(&dir, &home, true, false, &host, shipped());
    assert!(
        matches!(&steps[0], Step::Changed(t) if t.contains("earlier plist was replaced and kept as")),
        "{steps:?}"
    );
    assert_eq!(
        std::fs::read_to_string(plist.with_extension("plist.bak")).unwrap(),
        ORIGINAL
    );
    assert_ne!(std::fs::read_to_string(&plist).unwrap(), ORIGINAL);
}

#[test]
fn a_port_probe_that_fails_is_never_read_as_free() {
    let (_fx, dir, home) = setup();
    let host = Fake {
        probe_fails: true,
        agent_answers: true,
        ..Fake::default()
    };
    let steps = agent::steps(&dir, &home, true, true, &host, shipped());
    assert!(
        matches!(&steps[0], Step::Skipped(t) if t.contains("could not check") && t.contains("nothing was stopped")),
        "{steps:?}"
    );
    assert!(host.calls().is_empty() && !agent::plist_path(&home).exists());
}

#[test]
fn a_process_swapped_before_the_stop_is_left_alone() {
    let (_fx, dir, home) = setup();
    let host = Fake {
        agent_answers: true,
        ..Fake::default()
    };
    *host.listener.borrow_mut() = Some(known(&dir));
    *host.swapped.borrow_mut() = Some(Listener {
        pid: 9999,
        ..known(&dir)
    });
    let steps = agent::steps(&dir, &home, true, true, &host, shipped());
    assert!(
        matches!(&steps[0], Step::Failed(t) if t.contains("changed while preparing") && t.contains("nothing was stopped")),
        "{steps:?}"
    );
    assert!(host.calls().is_empty(), "{:?}", host.calls());
}

#[test]
fn an_absolute_interpreter_path_is_still_known() {
    let (_fx, dir, _home) = setup();
    let host = Fake::default();
    let abs = dir.join(SCRIPT).display().to_string();
    let l = Listener {
        pid: 4242,
        argv: vec![
            "/opt/homebrew/bin/python3".into(),
            abs,
            "--port".into(),
            "7890".into(),
        ],
        cwd: Some(dir.clone()),
    };
    *host.listener.borrow_mut() = Some(l.clone());
    assert_eq!(
        agent::classify(&dir, 7890, &host, shipped()),
        Existing::KnownManaged(l)
    );
}

/// A temp HOME (a test, a sandbox) never reaches launchd: one plain Skipped
/// line, no plist written; off macOS the line says launchd is needed.
#[test]
fn a_non_login_home_gets_one_skipped_line_and_no_files() {
    let (_fx, dir, home) = setup();
    let steps = agent::login_steps(&dir, &home, true, true, shipped());
    let expect = if cfg!(target_os = "macos") {
        "not this user's home folder"
    } else {
        "needs macOS"
    };
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(
        matches!(&steps[0], Step::Skipped(t) if t.contains(expect)),
        "{steps:?}"
    );
    assert!(!agent::plist_path(&home).exists());
    assert!(!agent::log_dir(&dir).exists());
}
