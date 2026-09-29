//! Tests for the `--session` override of `tm fleet init|status` (#8878 R1).
//!
//! Every test runs under a temp home, launches nothing, and reads tmux
//! through a stub [`Probe`]; a thread-local log records which session names
//! the probe was asked about.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use trusty_mpm::core::architect_session::record_launch;

use super::super::launch::PaneState;
use super::super::status::{StatusReport, status};
use super::super::tests::NO_TMUX;
use super::super::{
    InitReport, Probe, Step, init, init_with_session, resolve_dir, user_config_path,
};
use super::SessionNames;

thread_local! {
    /// The session names a [`LOGGING`] probe was asked about, in order.
    static ASKED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn log(name: &str) {
    ASKED.with(|a| a.borrow_mut().push(name.to_owned()));
}

/// Drain [`ASKED`].
fn asked() -> Vec<String> {
    ASKED.with(|a| std::mem::take(&mut *a.borrow_mut()))
}

/// No session runs; every name asked about is logged.
const LOGGING: Probe = Probe {
    pane: |name| {
        log(name);
        PaneState::Absent
    },
    stamp: |name| {
        log(name);
        None
    },
    claude: |name| {
        log(name);
        None
    },
};

/// Every session runs live, and its `claude` is this test process.
const LIVE_ME: Probe = Probe {
    pane: |_| PaneState::Live(PathBuf::from("/fleet-stub/any")),
    stamp: |_| None,
    claude: |_| Some(std::process::id()),
};

/// A scratch home and its default Architect directory.
struct Home {
    home: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().expect("scratch home"),
        }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn dir(&self) -> PathBuf {
        resolve_dir(None, self.home()).expect("default dir")
    }

    fn config(&self) -> String {
        std::fs::read_to_string(user_config_path(self.home())).unwrap_or_default()
    }

    fn write_config(&self, text: &str) {
        let path = user_config_path(self.home());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("config dir");
        std::fs::write(path, text).expect("write config");
    }

    fn init(&self, session: Option<&str>, probe: Probe) -> anyhow::Result<InitReport> {
        let names = session.map(|s| SessionNames::new(s).expect("valid name"));
        init_with_session(&self.dir(), self.home(), false, probe, names.as_ref())
    }

    fn status(&self, session: Option<&str>, probe: Probe) -> StatusReport {
        let names = session.map(|s| SessionNames::new(s).expect("valid name"));
        status(&self.dir(), self.home(), probe, names.as_ref())
    }
}

fn check<'a>(report: &'a StatusReport, name: &str) -> &'a super::super::status::Check {
    report
        .checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no {name} check in {}", report.render()))
}

#[test]
fn a_bad_session_flag_is_refused_before_any_write() {
    for bad in ["", "tm:arch", "tm.arch", "tm arch", "-tm", &"a".repeat(65)] {
        let err = SessionNames::new(bad).expect_err(bad);
        assert!(format!("{err:#}").contains("invalid --session"), "{err:#}");
    }
    let names = SessionNames::new("tm-supervisor").expect("valid");
    assert_eq!(
        (names.architect(), names.poll()),
        ("tm-supervisor", "tm-supervisor-poll")
    );
    let default = SessionNames::default_names();
    assert_eq!(
        (default.architect(), default.poll()),
        ("tm-architect", "tm-architect-poll")
    );
}

/// Item 1: the chosen name is recorded, and a later `init` and `status`
/// without the flag ask tmux about that name only.
#[test]
fn init_with_a_session_records_it_and_status_reads_it_back() {
    let h = Home::new();
    let report = h.init(Some("tm-supervisor"), LOGGING).expect("init");
    assert!(
        report
            .steps
            .iter()
            .any(|s| matches!(s, Step::Changed(t) if t.contains("`[supervisor] session = \"tm-supervisor\"`"))),
        "{}",
        report.render()
    );
    assert!(
        h.config().contains("session = \"tm-supervisor\""),
        "{}",
        h.config()
    );
    asked();

    let again = h.init(None, LOGGING).expect("second init");
    assert!(!again.changed(), "{}", again.render());
    assert!(asked().iter().all(|n| n == "tm-supervisor"));

    let report = h.status(None, LOGGING);
    assert_eq!(report.session, "tm-supervisor");
    assert!(
        check(&report, "session").detail.contains("tm-supervisor"),
        "{}",
        report.render()
    );
    let seen = asked();
    assert!(
        !seen.is_empty() && seen.iter().all(|n| n == "tm-supervisor"),
        "{seen:?}"
    );

    // The same flag again changes nothing.
    let same = h.init(Some("tm-supervisor"), LOGGING).expect("same flag");
    assert!(!same.changed(), "{}", same.render());
}

/// Item 1: without the flag, nothing new is written and the names are the
/// defaults; the flag set to the default writes no key either.
#[test]
fn the_default_session_writes_no_key() {
    let h = Home::new();
    init(&h.dir(), h.home(), false, LOGGING).expect("init");
    assert!(!h.config().contains("session"), "{}", h.config());
    assert!(asked().iter().all(|n| n == "tm-architect"));
    assert_eq!(h.status(None, NO_TMUX).session, "tm-architect");

    let report = h.init(Some("tm-architect"), NO_TMUX).expect("default flag");
    assert!(!report.changed(), "{}", report.render());
    assert!(!h.config().contains("session"), "{}", h.config());
}

#[test]
fn status_without_the_flag_reads_the_recorded_name() {
    let h = Home::new();
    assert_eq!(h.status(None, NO_TMUX).session, "tm-architect");
    h.write_config("[supervisor]\nsession = \"tm-supervisor\"\n");
    assert_eq!(h.status(None, NO_TMUX).session, "tm-supervisor");
    assert_eq!(h.status(Some("other"), NO_TMUX).session, "other");
}

/// Item 6: a new name refuses while a recorded session exists or tmux
/// cannot say, and leaves the config byte-identical.
#[test]
fn a_rename_is_refused_while_the_recorded_session_runs() {
    let h = Home::new();
    h.init(Some("tm-supervisor"), NO_TMUX).expect("first init");
    let before = h.config();
    let live = Probe {
        pane: |_| PaneState::Live(PathBuf::from("/fleet-stub/any")),
        ..NO_TMUX
    };
    let unknown = Probe {
        pane: |_| PaneState::Unknown("server exited".to_owned()),
        ..NO_TMUX
    };
    for (probe, why) in [(live, "still exists"), (unknown, "cannot be read")] {
        let err = h.init(Some("other"), probe).expect_err(why);
        assert!(format!("{err:#}").contains(why), "{err:#}");
        assert_eq!(h.config(), before, "{why}: the config changed");
    }
    h.init(Some("other"), NO_TMUX)
        .expect("rename with nothing running");
    assert!(h.config().contains("session = \"other\""), "{}", h.config());
}

/// Items 2 and 7: a recorded name that is not valid refuses `init` before
/// any write, and `status` reports it instead of falling back.
#[test]
fn a_malformed_recorded_name_refuses_init_and_fails_status() {
    for text in [
        "[supervisor]\nsession = \"tm:arch\"\n",
        "[supervisor]\nsession = 5\n",
    ] {
        let h = Home::new();
        h.write_config(text);
        let err = h.init(None, NO_TMUX).expect_err(text);
        assert!(format!("{err:#}").contains("supervisor.session"), "{err:#}");
        assert_eq!(h.config(), text);
        assert!(!h.dir().exists(), "init created the project");

        let report = h.status(None, LIVE_ME);
        assert_eq!(report.session, "");
        for name in ["session", "launch_stamp"] {
            let c = check(&report, name);
            assert!(
                !c.ok && c.detail.contains("recorded session name"),
                "{}",
                report.render()
            );
        }
        assert!(!report.binding.ok, "{}", report.render());
        assert!(!report.complete);
        // An explicit flag does not read the recorded name.
        assert_eq!(
            h.status(Some("tm-supervisor"), NO_TMUX).session,
            "tm-supervisor"
        );
    }
}

/// Item 7 Fail-Open Check: bound only with a launch record for the session's
/// `claude` that names the same session; a missing record, a tmux lookup
/// failure, an unreadable name and a mismatch are each not bound.
#[test]
fn status_binding_needs_the_record_and_the_same_session() {
    let h = Home::new();
    init(&h.dir(), h.home(), false, NO_TMUX).expect("init");
    let binding = |session: Option<&str>, probe: Probe| h.status(session, probe).binding;
    let unbound = |c: super::super::status::Check, why: &str| {
        assert!(!c.ok && c.detail.contains(why), "{c:?}");
    };

    unbound(binding(None, LIVE_ME), "no Architect launch record");
    let root = h.home().join(".trusty-mpm");
    record_launch(&root, std::process::id(), &h.dir(), "tm-architect").expect("record");
    let ok = binding(None, LIVE_ME);
    assert!(ok.ok, "{ok:?}");
    assert!(
        !ok.detail.contains(&std::process::id().to_string()),
        "{ok:?}"
    );

    unbound(
        binding(Some("tm-supervisor"), LIVE_ME),
        "names tmux session tm-architect",
    );
    let no_claude = Probe {
        claude: |_| None,
        ..LIVE_ME
    };
    unbound(binding(None, no_claude), "no `claude` process");
    let unknown = Probe {
        pane: |_| PaneState::Unknown("server exited".to_owned()),
        ..LIVE_ME
    };
    unbound(binding(None, unknown), "cannot read tmux session");
    unbound(binding(None, NO_TMUX), "is not running");

    let sidecar = root
        .join("architect-launch")
        .join(format!("{}.session", std::process::id()));
    std::fs::write(&sidecar, "{}").expect("corrupt the sidecar");
    unbound(binding(None, LIVE_ME), "could not be read");
}

/// Item 3: the poller starts with the chosen names in its environment.
#[test]
fn the_start_command_carries_the_chosen_session_names() {
    let env = |names: &SessionNames| {
        let cmd = super::super::poller::start_command(Path::new("/fleet-stub/a"), names);
        let get = |key: &str| {
            cmd.get_envs()
                .find(|(k, _)| *k == key)
                .and_then(|(_, v)| v)
                .map(|v| v.to_string_lossy().into_owned())
        };
        (get("ARCHITECT_SESSION"), get("ARCHITECT_POLL_SESSION"))
    };
    let chosen = SessionNames::new("tm-supervisor").expect("valid");
    assert_eq!(
        env(&chosen),
        (
            Some("tm-supervisor".to_owned()),
            Some("tm-supervisor-poll".to_owned())
        )
    );
    assert_eq!(
        env(&SessionNames::default_names()),
        (
            Some("tm-architect".to_owned()),
            Some("tm-architect-poll".to_owned())
        )
    );
}
