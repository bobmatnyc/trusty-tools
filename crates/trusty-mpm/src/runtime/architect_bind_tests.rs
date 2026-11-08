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
/// files skipped and an inline `mod tests` cut off.
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
            let text = std::fs::read_to_string(&path).ok()?;
            let cut = text
                .find("\n#[cfg(test)]\nmod tests {")
                .unwrap_or(text.len());
            Some((rel, text[..cut].to_owned()))
        })
        .collect()
}

/// The production files whose code (comments skipped) calls `needle`.
fn callers(sources: &[(String, String)], needle: &str) -> Vec<String> {
    sources
        .iter()
        .filter(|(_, text)| {
            text.lines().any(|line| {
                let line = line.trim_start();
                !line.starts_with("//") && line.contains(needle) && !line.contains("fn ")
            })
        })
        .map(|(rel, _)| rel.clone())
        .collect()
}

/// #8878: no tm path starts an Architect `claude` without binding it. Every
/// pane launch is typed by `send_spec_launch` (the only caller of the line
/// typer), which binds an Architect spec; only the fleet path skips that, and
/// it binds through `bind_architect`, the one place a launch record is written.
#[test]
fn every_architect_spawn_site_binds_8878() {
    let sources = production_sources();
    let only = |needle: &str, allowed: &[&str]| {
        let found = callers(&sources, needle);
        let stray: Vec<_> = found
            .iter()
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
        &["runtime/cli_launch.rs", "bin/tm/commands/fleet/launch.rs"],
    );
    only("record_launch(", &["core/architect_session.rs"]);
    only("record_architect(", &[]);
    only(
        "ARCHITECT_RECORDS.record_claude(",
        &["core/architect_session.rs", "core/architect_launch.rs"],
    );
    let text = |rel: &str| {
        sources
            .iter()
            .find(|(r, _)| r == rel)
            .map(|(_, t)| t.clone())
            .unwrap_or_else(|| panic!("{rel} missing"))
    };
    let cli = text("runtime/cli_launch.rs");
    let body = cli
        .split("pub fn send_spec_launch(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("send_spec_launch body");
    assert!(
        body.contains("bind_live("),
        "send_spec_launch does not bind"
    );
    assert!(
        text("bin/tm/commands/fleet/launch.rs").contains("bind_architect("),
        "the fleet launch path does not bind through bind_architect"
    );
}
