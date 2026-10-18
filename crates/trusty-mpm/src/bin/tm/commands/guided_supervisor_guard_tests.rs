//! Tests for [`super::refusal`] (#9092): each supervisor signal refuses on its
//! own, an unreadable source refuses, and a PM session is not refused.

use std::path::{Path, PathBuf};

use super::refusal;
use trusty_mpm::core::session_profile::ARCHITECT_WRITTEN_PROFILE_ID;

fn no_stamp(_: &str) -> Option<String> {
    None
}

fn supervisor_stamp(_: &str) -> Option<String> {
    Some(ARCHITECT_WRITTEN_PROFILE_ID.to_owned())
}

/// A scratch `~/.trusty-mpm` root and a project directory beside it.
struct Fixture {
    tmp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("root")).unwrap();
        std::fs::create_dir_all(tmp.path().join("project")).unwrap();
        Self { tmp }
    }

    fn root(&self) -> PathBuf {
        self.tmp.path().join("root")
    }

    fn project(&self) -> PathBuf {
        std::fs::canonicalize(self.tmp.path().join("project")).unwrap()
    }

    fn write_config(&self, body: &str) {
        std::fs::write(self.root().join("config.toml"), body).unwrap();
    }

    fn write_sidecar(&self, pid: u32, body: &str) {
        let dir = self.root().join("architect-launch");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{pid}.architect-session")), body).unwrap();
    }

    fn refusal(&self, session: &str, stamp: super::StampReader) -> Option<String> {
        refusal(session, Some(&self.project()), &self.root(), stamp)
    }
}

fn sidecar(session: &str) -> String {
    format!(r#"{{"pid": 4242, "start_time": 7, "session": "{session}"}}"#)
}

fn assert_refused(why: Option<String>, signal: &str) {
    let why = why.unwrap_or_else(|| panic!("not refused; expected the {signal} signal"));
    assert!(why.contains(signal), "{why}");
    assert!(why.contains("Run `tm fleet relaunch` instead"), "{why}");
}

#[test]
fn an_allow_listed_project_is_refused() {
    let fx = Fixture::new();
    fx.write_config(&format!(
        "[supervisor]\nprojects = [{:?}]\n",
        fx.project().display().to_string()
    ));
    assert_refused(
        fx.refusal("tm-pm", no_stamp),
        "is in the operator's Architect allowlist",
    );
}

#[test]
fn a_supervisor_stamp_alone_is_refused() {
    let fx = Fixture::new();
    assert_refused(
        fx.refusal("tm-pm", supervisor_stamp),
        "supervisor launch stamp",
    );
}

#[test]
fn a_recorded_session_name_alone_is_refused() {
    let fx = Fixture::new();
    fx.write_sidecar(4242, &sidecar("tm-sup"));
    assert_refused(
        fx.refusal("tm-sup", no_stamp),
        "an Architect launch record names the session",
    );
}

#[test]
fn an_unreadable_config_or_sidecar_refuses() {
    let fx = Fixture::new();
    fx.write_config("[supervisor\nprojects = ");
    assert_refused(
        fx.refusal("tm-pm", no_stamp),
        "the user config cannot be read",
    );

    let fx = Fixture::new();
    fx.write_sidecar(4242, "{not json");
    assert_refused(
        fx.refusal("tm-pm", no_stamp),
        "the Architect launch records cannot be read",
    );
}

#[test]
fn a_pm_session_is_not_refused() {
    let fx = Fixture::new();
    assert_eq!(fx.refusal("tm-pm", no_stamp), None);

    // Another project's grant and another session's record do not mark it.
    let other = fx.tmp.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    fx.write_config(&format!(
        "[supervisor]\nprojects = [{:?}]\n",
        std::fs::canonicalize(&other).unwrap().display().to_string()
    ));
    fx.write_sidecar(4242, &sidecar("tm-architect"));
    assert_eq!(fx.refusal("tm-pm", no_stamp), None);
    assert_eq!(refusal("tm-pm", None::<&Path>, &fx.root(), no_stamp), None);
}
