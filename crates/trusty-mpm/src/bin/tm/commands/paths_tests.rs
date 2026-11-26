//! Tests for `tm paths show|set` (#11821 P6b), over a temp home with the
//! env, the session markers and the live-session probe injected.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::Parser;
use trusty_common::install_paths::{
    InstallPathsError, PathSource, install_file_path, read_install_file,
};

use super::{SetRequest, set, show};
use crate::cli::{Cli, Command, PathsAction};

/// An env lookup holding exactly `vars`.
fn vars(vars: Vec<(&'static str, &'static str)>) -> impl Fn(&str) -> Option<String> {
    move |n| {
        vars.iter()
            .find(|(k, _)| *k == n)
            .map(|(_, v)| (*v).to_owned())
    }
}

/// An operator shell: no Claude Code marker, no agent marker.
fn operator(_: &str) -> Option<OsString> {
    None
}

/// No live tm session.
fn none_live() -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

/// A request for `--projects-root <p>`.
fn root(p: &str) -> SetRequest {
    SetRequest {
        projects_root: Some(PathBuf::from(p)),
        config_dir: None,
    }
}

/// Write `body` as `home`'s install file.
fn install(home: &Path, body: &str) {
    let file = install_file_path(home);
    std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
    std::fs::write(file, body).expect("write install.toml");
}

#[test]
fn cli_parses_paths_show_and_set() {
    let parse = |argv: &[&str]| match Cli::try_parse_from(argv).expect("parses").command {
        Some(Command::Paths { action }) => action,
        other => panic!("{argv:?} parsed as {other:?}"),
    };
    assert!(matches!(parse(&["tm", "paths", "show"]), PathsAction::Show));
    let set = parse(&[
        "tm",
        "paths",
        "set",
        "--projects-root",
        "/p",
        "--config-dir",
        "/c",
    ]);
    assert!(matches!(
        set,
        PathsAction::Set { projects_root: Some(ref p), config_dir: Some(ref c) }
            if p == Path::new("/p") && c == Path::new("/c")
    ));
}

#[test]
fn show_prints_each_path_with_its_source() {
    let home = tempfile::tempdir().expect("home");
    let h = home.path();
    let cfg = h.join("cfg");
    install(h, &format!("config_dir = \"{}\"\n", cfg.display()));
    let env = vars(vec![("TRUSTY_MPM_WORKSPACE_ROOT", "/env/projects")]);
    let out = show(h, env, || None).expect("shows");
    assert!(out.contains("(present)"), "{out}");
    assert!(out.contains("/env/projects (env)"), "{out}");
    assert!(
        out.contains(&format!("{} (install-file)", cfg.display())),
        "{out}"
    );
    assert!(out.contains("claude_config_dir  unset"), "{out}");

    // No file: the template rung, then the default, each named.
    let bare = tempfile::tempdir().expect("home");
    let out = show(bare.path(), vars(vec![]), || Some("/tpl/root".into())).expect("shows");
    assert!(out.contains("(absent)"), "{out}");
    assert!(
        out.contains(&format!("/tpl/root ({})", PathSource::Template)),
        "{out}"
    );
    let dflt = format!("{} ({})", bare.path().display(), PathSource::Default);
    assert!(out.contains(&dflt), "{out}");
}

/// #11821 P6b: a broken install.toml shows its error and no `$HOME` default,
/// even when env decides both keys.
#[test]
fn show_on_a_broken_install_file_is_the_error_not_the_default() {
    let home = tempfile::tempdir().expect("home");
    let h = home.path();
    install(h, "project_root = \"/typo\"\n");
    let both = vars(vec![
        ("TRUSTY_MPM_WORKSPACE_ROOT", "/env/projects"),
        ("TRUSTY_MPM_CONFIG_DIR", "/env/cfg"),
    ]);
    for env in [
        Box::new(vars(vec![])) as Box<dyn Fn(&str) -> Option<String>>,
        Box::new(both),
    ] {
        let err = format!(
            "{:#}",
            show(h, env, || None).expect_err("a broken file errors")
        );
        assert!(err.contains("is broken or unreadable"), "{err}");
        assert!(
            err.contains("project_root"),
            "the parse error is shown: {err}"
        );
        assert!(!err.contains("trusty-mpm-projects"), "no default: {err}");
    }
}

#[test]
fn set_writes_through_the_writer_and_keeps_other_keys() {
    let home = tempfile::tempdir().expect("home");
    let h = home.path();
    install(h, "codex_home = \"/codex\"\nconfig_dir = \"/old/cfg\"\n");
    let new_root = h.join("new-projects");
    let req = root(&new_root.display().to_string());
    let out = set(&req, h, operator, none_live).expect("writes");
    assert!(out.contains("projects_root: unset ->"), "{out}");
    assert!(out.contains("Nothing was moved"), "{out}");
    let file = read_install_file(h).expect("reads").expect("present");
    assert_eq!(file.projects_root.as_deref(), Some(new_root.as_path()));
    assert_eq!(file.config_dir.as_deref(), Some(Path::new("/old/cfg")));
    assert_eq!(file.codex_home.as_deref(), Some(Path::new("/codex")));
    // #11821 D1: nothing is created at, or moved to, the new path.
    assert!(!new_root.exists(), "set must not create or move data");
}

#[test]
fn set_refuses_a_relative_path_with_the_typed_error() {
    let home = tempfile::tempdir().expect("home");
    let h = home.path();
    for req in [
        root("relative/projects"),
        SetRequest {
            projects_root: None,
            config_dir: Some("cfg".into()),
        },
    ] {
        let err = set(&req, h, operator, none_live).expect_err("relative refused");
        let typed = err.downcast_ref::<InstallPathsError>();
        assert!(
            matches!(
                typed,
                Some(InstallPathsError::Relative {
                    origin: PathSource::InstallFile,
                    ..
                })
            ),
            "{err:#}"
        );
    }
    assert!(!install_file_path(h).exists(), "nothing written");
}

/// #11821 P6b owner-only: a Claude Code Bash child carries the markers.
#[test]
fn set_refuses_inside_a_claude_code_session() {
    let home = tempfile::tempdir().expect("home");
    for marker in ["CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "CLAUDE_PID"] {
        let env = move |n: &str| {
            (n == marker).then(|| OsString::from(if marker == "CLAUDE_PID" { "42" } else { "1" }))
        };
        let live_called = std::cell::Cell::new(false);
        let err = set(&root("/abs"), home.path(), env, || {
            live_called.set(true);
            Ok(Vec::new())
        })
        .expect_err("a session refuses")
        .to_string();
        assert!(err.contains("tm paths set refused"), "{err}");
        assert!(err.contains(&format!("`{marker}`")), "{err}");
        assert!(err.contains("install.toml was not written"), "{err}");
        assert!(!live_called.get(), "refused before anything else is read");
    }
    assert!(!install_file_path(home.path()).exists());
}

#[test]
fn set_refuses_an_agent() {
    let home = tempfile::tempdir().expect("home");
    let env = |n: &str| (n == crate::commands::misc::SUB_AGENT_ENV).then(|| OsString::from("1"));
    let err = set(&root("/abs"), home.path(), env, none_live)
        .expect_err("an agent refuses")
        .to_string();
    assert!(err.contains("an agent runs it"), "{err}");
    assert!(!install_file_path(home.path()).exists());
}

/// #11821 D1: no write while sessions are live.
#[test]
fn set_refuses_while_sessions_are_live() {
    let home = tempfile::tempdir().expect("home");
    let live = || Ok(vec!["tm-alpha".to_owned(), "tm-ws-x-1:pm".to_owned()]);
    let err = set(&root("/abs"), home.path(), operator, live)
        .expect_err("live sessions refuse")
        .to_string();
    assert!(
        err.contains("2 tm session(s) are live (tm-alpha, tm-ws-x-1:pm)"),
        "{err}"
    );
    assert!(!install_file_path(home.path()).exists());
}

#[test]
fn set_refuses_when_live_sessions_cannot_be_read() {
    let home = tempfile::tempdir().expect("home");
    let err = set(&root("/abs"), home.path(), operator, || {
        Err("tmux timed out".into())
    })
    .expect_err("an unknown live set refuses")
    .to_string();
    assert!(
        err.contains("cannot tell whether tm sessions are live (tmux timed out)"),
        "{err}"
    );
    assert!(!install_file_path(home.path()).exists());
}

#[test]
fn set_refuses_to_merge_over_a_broken_install_file() {
    let home = tempfile::tempdir().expect("home");
    let h = home.path();
    install(h, "not toml [\n");
    let err = format!(
        "{:#}",
        set(&root("/abs"), h, operator, none_live).expect_err("broken")
    );
    assert!(err.contains("is broken or unreadable"), "{err}");
    let body = std::fs::read_to_string(install_file_path(h)).expect("still there");
    assert_eq!(
        body, "not toml [\n",
        "the broken file is left byte-identical"
    );
}

#[test]
fn set_without_a_flag_is_refused() {
    let home = tempfile::tempdir().expect("home");
    let err = set(&SetRequest::default(), home.path(), operator, none_live)
        .expect_err("no flag")
        .to_string();
    assert!(
        err.contains("needs --projects-root, --config-dir or both"),
        "{err}"
    );
}
