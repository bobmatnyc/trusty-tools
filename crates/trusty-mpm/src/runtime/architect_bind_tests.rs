//! Tests for the #8878 Architect bind (`architect_bind.rs`).

use std::path::{Path, PathBuf};

use super::*;
use crate::core::architect_launch::check_launched_architect;
use crate::core::session_profile::{SessionProfile, launch_env};
use crate::core::twin_arming::process_facts;
use crate::core::twin_identity::ClaudeProcess;

/// A spec started in `cwd` whose env sets each of `profiles`' stamps in order.
fn spec(cwd: &Path, profiles: &[SessionProfile]) -> LaunchSpec {
    LaunchSpec {
        session_id: String::new(),
        launch_id: String::new(),
        cwd: cwd.to_path_buf(),
        program: "claude".to_owned(),
        args: Vec::new(),
        env_unset: Vec::new(),
        env_set: profiles.iter().map(|p| launch_env(*p)).collect(),
    }
}

/// A live child process standing in for the launched `claude`.
struct Live(std::process::Child);

impl Live {
    fn spawn() -> Self {
        Self(
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("spawn sleep"),
        )
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn scratch() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join(".trusty-mpm");
    let project = dir.path().join("architect");
    std::fs::create_dir_all(&project).expect("mkdir project");
    (dir, root, project)
}

/// #8878: an Architect spec binds the `claude` found in its session, and the
/// record is one the trust-anchor identity check accepts; a PM spec, or one
/// whose last stamp is the PM's, binds nothing and never looks for a process.
#[test]
fn only_an_architect_stamped_spec_is_bound() {
    let (_dir, root, project) = scratch();
    let never = || -> Option<u32> { panic!("a PM launch must not look for a claude") };
    for profiles in [
        vec![],
        vec![SessionProfile::Pm],
        vec![SessionProfile::Architect, SessionProfile::Pm],
    ] {
        let got = bind_launch(Some(&root), &spec(&project, &profiles), "tm-x", never);
        assert_eq!(got, Binding::NotArchitect, "{profiles:?}");
    }
    assert!(!root.exists(), "nothing recorded for a PM launch");

    let claude = Live::spawn();
    let architect = spec(&project, &[SessionProfile::Architect]);
    let Binding::Bound(record) =
        bind_launch(Some(&root), &architect, "tm-x", || Some(claude.pid()))
    else {
        panic!("the Architect launch was not bound");
    };
    assert_eq!(record.pid, claude.pid());
    let live = ClaudeProcess {
        pid: claude.pid(),
        start_time: process_facts(claude.pid()).expect("facts").start_time,
    };
    assert_eq!(
        check_launched_architect(&root, &project, || Ok(Some(live))),
        Ok(())
    );
}

/// #8878 Fail-Open Check: every bind failure is unbound, never a record.
#[test]
fn an_architect_spec_with_no_claude_or_no_home_is_unbound() {
    let (_dir, root, project) = scratch();
    let architect = spec(&project, &[SessionProfile::Architect]);
    assert!(matches!(
        bind_launch(Some(&root), &architect, "tm-x", || None),
        Binding::NoClaude(_)
    ));
    assert!(matches!(
        bind_launch(None, &architect, "tm-x", || Some(1)),
        Binding::NotRecorded(_)
    ));
    // A session name tmux could misread is refused before any write.
    let claude = Live::spawn();
    assert!(matches!(
        bind_launch(Some(&root), &architect, "tm:x", || Some(claude.pid())),
        Binding::NotRecorded(_)
    ));
    assert!(!root.join("architect-launch").exists(), "no record written");
}

#[test]
fn an_unbound_architect_names_the_relaunch() {
    let warn = Binding::NoClaude("gone".to_owned())
        .warning("tm-a")
        .expect("warns");
    assert!(
        warn.contains("tm-a") && warn.contains("tm fleet relaunch"),
        "{warn}"
    );
    assert_eq!(Binding::NotArchitect.warning("tm-a"), None);
}

/// #8878: `bind_architect` is the one launch-record writer, and its record
/// passes the identity check for that process and project only.
#[test]
fn bind_architect_writes_a_record_the_identity_check_accepts() {
    let (_dir, root, project) = scratch();
    let claude = Live::spawn();
    let record = bind_architect(&root, claude.pid(), &project, "tm-architect").expect("bind");
    let live = ClaudeProcess {
        pid: claude.pid(),
        start_time: record.start_time,
    };
    assert_eq!(
        check_launched_architect(&root, &project, || Ok(Some(live))),
        Ok(())
    );
    let elsewhere = root.join("elsewhere");
    assert!(check_launched_architect(&root, &elsewhere, || Ok(Some(live))).is_err());
}

/// Production source of this crate: `(path relative to src, text)`, with test
/// files skipped and every top-level inline `#[cfg(test)] mod … { … }` cut.
fn production_sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read src dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&src, &mut files);
    files
        .into_iter()
        .filter_map(|path| {
            let rel = path.strip_prefix(&src).ok()?.to_string_lossy().into_owned();
            let name = path.file_name()?.to_string_lossy().into_owned();
            if name == "tests.rs" || name.ends_with("_tests.rs") || rel.contains("/tests/") {
                return None;
            }
            Some((
                rel,
                without_test_mods(&std::fs::read_to_string(&path).ok()?),
            ))
        })
        .collect()
}

/// `text` with each top-level `#[cfg(test)] mod <name> {` block removed.
fn without_test_mods(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("\n#[cfg(test)]\nmod ") {
        let head = &rest[at + 1..];
        let opens = head.lines().nth(1).is_some_and(|l| l.ends_with(" {"));
        let Some(close) = head.find("\n}\n").filter(|_| opens) else {
            out.push_str(&rest[..at + 1]);
            rest = head;
            continue;
        };
        out.push_str(&rest[..at + 1]);
        rest = &head[close + 3..];
    }
    out.push_str(rest);
    out
}

/// Whether `line` calls `needle` as a whole name (`launch_env(` is not a call
/// in `pinned_relaunch_env(`).
fn calls(line: &str, needle: &str) -> bool {
    line.match_indices(needle).any(|(at, _)| {
        !line[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// The production files whose code (comments skipped) calls `needle`.
fn callers(sources: &[(String, String)], needle: &str) -> Vec<String> {
    sources
        .iter()
        .filter(|(_, text)| {
            text.lines().any(|line| {
                let line = line.trim_start();
                !line.starts_with("//") && calls(line, needle) && !line.contains("fn ")
            })
        })
        .map(|(rel, _)| rel.clone())
        .collect()
}

/// Whether `text`'s code (comment lines skipped) contains `needle`.
fn code_has(text: &str, needle: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_start();
        !line.starts_with("//") && line.contains(needle)
    })
}

/// How a production file that stamps or builds a launch profile treats an
/// Architect `claude` it could start.
enum Site {
    /// Binds it: its code calls this bind (or a function that binds).
    Binds(&'static str),
    /// Builds the stamp for the callers of this function, each listed too.
    Builds(&'static str),
    /// Refuses a supervisor session before it starts one (#9092).
    Refuses(&'static str),
    /// Does not bind yet; named in [`DEFERRED`].
    Deferred,
    /// Starts no `claude`: defines the stamp, or a doctor probe line.
    NoLaunch,
}

/// The calls that put a profile stamp on a launch, or carry one to a launch.
const STAMP_NEEDLES: &[&str] = &[
    "launch_env(",
    "\"TRUSTY_MPM_SESSION_PROFILE\"",
    "cli_launch(",
    "gui_spawn_line(",
    "spawn_spec(",
    "relaunch_spec(",
];

/// Every production file a [`STAMP_NEEDLES`] call sits in, and its [`Site`].
const SITES: &[(&str, Site)] = &[
    ("core/session_profile.rs", Site::NoLaunch),
    ("daemon/doctor_transcript_saving.rs", Site::NoLaunch),
    (
        "core/session_launch/system_prompt.rs",
        Site::Builds("cli_launch("),
    ),
    ("daemon/spawn_command.rs", Site::Builds("gui_spawn_line(")),
    ("runtime/managed_launch.rs", Site::Binds("bind_delivered(")),
    (
        "runtime/claude_code.rs",
        Site::Binds("managed_launch::deliver_in("),
    ),
    (
        "daemon/services/tmux_service.rs",
        Site::Binds("bind_profile("),
    ),
    (
        "bin/tm/commands/launch.rs",
        Site::Binds("send_spec_launch("),
    ),
    (
        "bin/tm/commands/session/start.rs",
        Site::Binds("send_spec_launch("),
    ),
    (
        "bin/tm/commands/fleet/launch.rs",
        Site::Binds("bind_architect("),
    ),
    (
        "bin/tm/commands/guided_inplace.rs",
        Site::Refuses("guided_supervisor_guard::refusal("),
    ),
    ("core/standalone/run.rs", Site::Deferred),
];

/// #8878: no tm path starts an Architect `claude` without binding it, unless
/// it refuses a supervisor session or is named in [`DEFERRED`]. Every file
/// that stamps a profile (or builds or carries a stamped launch) is listed in
/// [`SITES`], so a new stamp site fails here until it is bound or deferred.
/// The pane line typer is reached only through `send_spec_launch`, which
/// binds, and the unbound form only from the fleet path (which binds through
/// `bind_architect`) and a start into a session that was already running.
#[test]
fn every_architect_spawn_site_binds_8878() {
    let sources = production_sources();
    let text = |rel: &str| {
        sources
            .iter()
            .find(|(r, _)| r == rel)
            .map(|(_, t)| t.clone())
            .unwrap_or_else(|| panic!("{rel} missing"))
    };
    let mut found: Vec<String> = STAMP_NEEDLES
        .iter()
        .flat_map(|needle| callers(&sources, needle))
        .collect();
    found.sort();
    found.dedup();
    let mut listed: Vec<String> = SITES.iter().map(|(rel, _)| (*rel).to_owned()).collect();
    listed.sort();
    assert_eq!(found, listed, "a profile stamp site is unlisted or gone");
    let mut deferred = Vec::new();
    for (rel, site) in SITES {
        let code = text(rel);
        match site {
            Site::Binds(call) | Site::Refuses(call) => {
                assert!(code_has(&code, call), "{rel} does not call {call}");
            }
            Site::Builds(call) => assert!(STAMP_NEEDLES.contains(call), "{call} unscanned"),
            Site::Deferred => deferred.push(*rel),
            Site::NoLaunch => {}
        }
    }
    for rel in &deferred {
        assert!(DEFERRED.iter().any(|(path, _)| path.contains(rel)), "{rel}");
    }
    assert_eq!(
        deferred.len(),
        DEFERRED.len(),
        "DEFERRED names a bound path"
    );

    let only = |needle: &str, allowed: &[&str]| {
        let stray: Vec<_> = callers(&sources, needle)
            .into_iter()
            .filter(|f| !allowed.contains(&f.as_str()))
            .collect();
        assert!(
            stray.is_empty(),
            "`{needle}` called outside {allowed:?}: {stray:?}"
        );
    };
    only("send_launch_line(", &["runtime/cli_launch.rs"]);
    only("send_spec_launch_with(", &["runtime/cli_launch.rs"]);
    only(
        "send_spec_launch_unbound(",
        &[
            "runtime/cli_launch.rs",
            "bin/tm/commands/fleet/launch.rs",
            "bin/tm/commands/session/start.rs",
        ],
    );
    only("deliver_in(", &["runtime/claude_code.rs"]);
    only("record_launch(", &["core/architect_session.rs"]);
    only("record_architect(", &[]);
    only(
        "ARCHITECT_RECORDS.record_claude(",
        &["core/architect_session.rs", "core/architect_launch.rs"],
    );
    let body = |rel: &str, head: &str| {
        text(rel)
            .split(head)
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next().map(str::to_owned))
            .unwrap_or_else(|| panic!("{head} in {rel}"))
    };
    let typer = body("runtime/cli_launch.rs", "pub fn send_spec_launch(");
    assert!(
        typer.contains("bind_live("),
        "send_spec_launch does not bind"
    );
    let deliver = body("runtime/managed_launch.rs", "fn deliver_in(");
    assert!(
        deliver.contains("bind_delivered("),
        "deliver_in does not bind"
    );
}

/// #8878 critic: a start into a session that was already running sends the
/// launch unbound and records nothing, as `tm connect` starts nothing there;
/// the GUI spawn does the same for a session it attached to.
#[test]
fn a_reused_session_binds_nothing_8878() {
    assert_eq!(bind_reused(false), Binding::NotArchitect);
    let reused = bind_reused(true);
    assert_eq!(reused, Binding::NotStarted);
    let warn = reused.warning("tm-a").expect("warns");
    assert!(warn.contains("already running"), "{warn}");
    let sources = production_sources();
    let start = &sources
        .iter()
        .find(|(rel, _)| rel == "bin/tm/commands/session/start.rs")
        .expect("start.rs")
        .1;
    let reused_arm = start
        .split("let sent = if already_running {")
        .nth(1)
        .and_then(|rest| rest.split("} else {").next())
        .expect("start binds by already_running");
    assert!(
        reused_arm.contains("send_spec_launch_unbound(") && reused_arm.contains("bind_reused("),
        "{reused_arm}"
    );
    let gui = &sources
        .iter()
        .find(|(rel, _)| rel == "daemon/services/tmux_service.rs")
        .expect("tmux_service.rs")
        .1;
    assert!(gui.contains("if already_running {\n            bind_reused(architect)"));
}
