//! Unit tests for the Architect's local install-path reader (#11821 P4-f2).

use std::path::{Path, PathBuf};

use super::file::parse;
use super::*;

/// A scratch home whose `.trusty-tools/install.toml` holds `body`, if given.
fn home_with(body: Option<&str>) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    if let Some(body) = body {
        let dir = home.path().join(TOOLS_DIR_NAME);
        std::fs::create_dir_all(&dir).expect("tools dir");
        std::fs::write(dir.join(INSTALL_FILE_NAME), body).expect("install file");
    }
    home
}

fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |k| {
        pairs
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| (*v).to_owned())
    }
}

#[test]
fn absent_file_is_none() {
    let home = home_with(None);
    assert_eq!(read_install_file(home.path()).expect("absent"), None);
}

#[test]
fn valid_spellings_parse() {
    let body = "# install\n\nconfig_dir = \"/c\\u0041\" # moved\n\"projects_root\" = '/p'\r\n\
                claude_config_dir=\"/a\\\\b\"\ncodex_home = \"\"\n";
    let file = parse(body).expect("parses");
    assert_eq!(file.config_dir.as_deref(), Some(Path::new("/cA")));
    assert_eq!(file.projects_root.as_deref(), Some(Path::new("/p")));
    assert_eq!(file.claude_config_dir.as_deref(), Some(Path::new("/a\\b")));
    assert_eq!(file.codex_home.as_deref(), Some(Path::new("")));
}

#[test]
fn broken_files_are_refused() {
    for body in [
        "config_dir = \"/x",
        "config_dir = /x",
        "config_dir = 5",
        "project_root = \"/x\"",
        "[paths]\nconfig_dir = \"/x\"",
        "config_dir = \"/x\"\nconfig_dir = \"/y\"",
        "config_dir = \"/x\" trailing",
        "config_dir \"/x\"",
        "config_dir = \"\"\"/x",
        "config_dir = \"/x\\q\"",
        "config_dir = \"/x\\e\"",
        "paths.config_dir = \"/x\"",
        "config_dir = \"/x\" # bad\u{1}",
        "config_dir = \"/x\"\r# lone CR",
        "# c\n\u{feff}config_dir = \"/x\"",
    ] {
        assert!(parse(body).is_err(), "{body:?} must be refused");
    }
}

/// #11821: the `toml` crate's multi-line rules; parity in trusty-crate-contracts.
#[test]
fn multi_line_strings_parse_as_toml_does() {
    for (body, want) in [
        ("config_dir = \"\"\"\n/ml\"\"\"", "/ml"),
        ("config_dir = \"\"\"/a\r\nb\"\"\"", "/a\nb"),
        ("config_dir = \"\"\"/j\\\n   \n  oin\"\"\"", "/join"),
        ("config_dir = \"\"\"/q\"\"x\"\"\"\"\"", "/q\"\"x\"\""),
        ("config_dir = '''/a\\b'''", "/a\\b"),
        ("\u{feff}config_dir = '/bom'", "/bom"),
    ] {
        let file = parse(body).unwrap_or_else(|e| panic!("{body:?}: {e}"));
        assert_eq!(
            file.config_dir.as_deref(),
            Some(Path::new(want)),
            "{body:?}"
        );
    }
}

#[test]
fn config_dir_env_beats_file_beats_home() {
    let home = home_with(Some("config_dir = \"/from/file\"\n"));
    let h = home.path();
    let by_env = config_dir(h, &env(&[(CONFIG_DIR_ENV, " ~/env ")])).expect("env");
    assert_eq!(
        (by_env.path, by_env.source),
        (h.join("env"), PathSource::Env)
    );
    let by_file = config_dir(h, &env(&[(CONFIG_DIR_ENV, "  ")])).expect("file");
    assert_eq!(
        (by_file.path, by_file.source),
        (PathBuf::from("/from/file"), PathSource::InstallFile)
    );
    let bare = home_with(None);
    let by_default = config_dir(bare.path(), &env(&[])).expect("default");
    assert_eq!(
        (by_default.path, by_default.source),
        (bare.path().to_path_buf(), PathSource::Default)
    );
}

/// #11821: a broken install file fails closed; red against a reader that
/// falls back to `home` on a parse or read error.
#[test]
fn broken_file_fails_closed_unless_env_decides() {
    let broken = home_with(Some("config_dir = \"/x\nnot toml\n"));
    let err = config_dir(broken.path(), &env(&[])).expect_err("broken file");
    assert!(matches!(err, InstallPathsError::Parse { .. }), "{err}");
    let by_env = config_dir(broken.path(), &env(&[(CONFIG_DIR_ENV, "/e")])).expect("env wins");
    assert_eq!(by_env.path, PathBuf::from("/e"));

    let unreadable = home_with(None);
    std::fs::create_dir_all(install_file_path(unreadable.path())).expect("dir as file");
    let err = config_dir(unreadable.path(), &env(&[])).expect_err("unreadable");
    assert!(matches!(err, InstallPathsError::Read { .. }), "{err}");

    let relative = home_with(Some("config_dir = \"rel/dir\"\n"));
    let err = config_dir(relative.path(), &env(&[])).expect_err("relative");
    assert!(
        matches!(
            err,
            InstallPathsError::Relative {
                origin: PathSource::InstallFile,
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn projects_root_template_sits_below_the_file() {
    let bare = home_with(None);
    let h = bare.path();
    let by_template = projects_root(h, &env(&[]), Some(" ~/tpl ")).expect("template");
    assert_eq!(
        (by_template.path, by_template.source),
        (h.join("tpl"), PathSource::Template)
    );
    let err = projects_root(h, &env(&[]), Some("tpl")).expect_err("relative template");
    assert!(matches!(err, InstallPathsError::Relative { .. }), "{err}");
    let filed = home_with(Some("projects_root = \"/p\"\n"));
    let by_file = projects_root(filed.path(), &env(&[]), Some("/tpl")).expect("file");
    assert_eq!(by_file.source, PathSource::InstallFile);
    let by_default = projects_root(h, &env(&[]), Some("  ")).expect("default");
    assert_eq!(by_default.path, h.join(DEFAULT_PROJECTS_DIR));
}
