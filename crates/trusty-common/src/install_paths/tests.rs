//! Tests for `install_paths` (#11821). Every test but the wrapper equivalence
//! test injects env and home; that one holds the crate-wide `ENV_LOCK`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::*;

/// An injected env lookup over fixed pairs.
fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    move |name| map.get(name).cloned()
}

fn no_env() -> impl Fn(&str) -> Option<String> {
    env_of(&[])
}

fn write_raw(home: &Path, body: &str) {
    let path = install_file_path(home);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, body).expect("write install.toml");
}

fn rp(path: &str, source: PathSource) -> ResolvedPath {
    ResolvedPath {
        path: PathBuf::from(path),
        source,
    }
}

const FULL_FILE: &str = "projects_root = \"/file/projects\"\n\
config_dir = \"/file/config\"\n\
claude_config_dir = \"/file/claude\"\n\
codex_home = \"/file/codex\"\n";

const FULL_ENV: [(&str, &str); 4] = [
    ("TRUSTY_MPM_WORKSPACE_ROOT", "/env/projects"),
    ("TRUSTY_MPM_CONFIG_DIR", "/env/config"),
    ("CLAUDE_CONFIG_DIR", "/env/claude"),
    ("CODEX_HOME", "/env/codex"),
];

/// Why: P1 must not move anything for an operator who configures nothing.
/// What: empty env, no file -> `~/trusty-mpm-projects`, config dir `~`,
/// `~/.trusty-tools`, `~/.trusty-mpm`, no Claude or Codex dir.
#[test]
fn defaults_match_today() {
    let home = tempfile::tempdir().expect("tempdir");
    let h = home.path();
    let got = InstallPaths::resolve_with(no_env(), h, &PathOverrides::default()).expect("resolve");
    assert_eq!(got.projects_root.path, h.join("trusty-mpm-projects"));
    assert_eq!(got.projects_root.source, PathSource::Default);
    assert_eq!(got.config_dir.path, h);
    assert_eq!(got.config_dir.source, PathSource::Default);
    assert_eq!(got.claude_config_dir, None);
    assert_eq!(got.codex_home, None);
}

/// What: both framework dirs hang off the resolved config dir.
#[test]
fn tools_and_mpm_dirs_sit_under_config_dir() {
    let home = tempfile::tempdir().expect("tempdir");
    let env = env_of(&[("TRUSTY_MPM_CONFIG_DIR", "/srv/cfg")]);
    let got =
        InstallPaths::resolve_with(env, home.path(), &PathOverrides::default()).expect("resolve");
    assert_eq!(got.tools_dir(), PathBuf::from("/srv/cfg/.trusty-tools"));
    assert_eq!(got.mpm_dir(), PathBuf::from("/srv/cfg/.trusty-mpm"));
}

/// What: a caller override beats env, the file and the default on every key.
#[test]
fn flag_beats_env_file_and_default() {
    let home = tempfile::tempdir().expect("tempdir");
    write_raw(home.path(), FULL_FILE);
    let flags = PathOverrides {
        projects_root: Some("/flag/projects".into()),
        config_dir: Some("/flag/config".into()),
        claude_config_dir: Some("/flag/claude".into()),
        codex_home: Some("/flag/codex".into()),
        projects_root_template: Some("/template/projects".into()),
    };
    let got = InstallPaths::resolve_with(env_of(&FULL_ENV), home.path(), &flags).expect("resolve");
    assert_eq!(got.projects_root, rp("/flag/projects", PathSource::Flag));
    assert_eq!(got.config_dir, rp("/flag/config", PathSource::Flag));
    assert_eq!(
        got.claude_config_dir,
        Some(rp("/flag/claude", PathSource::Flag))
    );
    assert_eq!(got.codex_home, Some(rp("/flag/codex", PathSource::Flag)));
}

/// What: env beats the file and the default on every key; `~` in env expands
/// against home; an empty env value counts as unset.
#[test]
fn env_beats_file_and_default() {
    let home = tempfile::tempdir().expect("tempdir");
    write_raw(home.path(), FULL_FILE);
    let got = InstallPaths::resolve_with(env_of(&FULL_ENV), home.path(), &PathOverrides::default())
        .expect("resolve");
    assert_eq!(got.projects_root, rp("/env/projects", PathSource::Env));
    assert_eq!(got.config_dir, rp("/env/config", PathSource::Env));
    assert_eq!(
        got.claude_config_dir,
        Some(rp("/env/claude", PathSource::Env))
    );
    assert_eq!(got.codex_home, Some(rp("/env/codex", PathSource::Env)));

    let env = env_of(&[
        ("TRUSTY_MPM_WORKSPACE_ROOT", "~/code"),
        ("TRUSTY_MPM_CONFIG_DIR", "   "),
    ]);
    let got =
        InstallPaths::resolve_with(env, home.path(), &PathOverrides::default()).expect("resolve");
    assert_eq!(got.projects_root.path, home.path().join("code"));
    assert_eq!(got.config_dir, rp("/file/config", PathSource::InstallFile));
}

/// What: the file beats the default on every key.
#[test]
fn file_beats_default() {
    let home = tempfile::tempdir().expect("tempdir");
    write_raw(home.path(), FULL_FILE);
    let got = InstallPaths::resolve_with(no_env(), home.path(), &PathOverrides::default())
        .expect("resolve");
    assert_eq!(
        got.projects_root,
        rp("/file/projects", PathSource::InstallFile)
    );
    assert_eq!(got.config_dir, rp("/file/config", PathSource::InstallFile));
    assert_eq!(
        got.claude_config_dir,
        Some(rp("/file/claude", PathSource::InstallFile))
    );
    assert_eq!(
        got.codex_home,
        Some(rp("/file/codex", PathSource::InstallFile))
    );
}

/// What: an absent file reads as `None`, and an empty file leaves every key
/// at its default.
#[test]
fn missing_file_means_default() {
    let home = tempfile::tempdir().expect("tempdir");
    assert_eq!(read_install_file(home.path()).expect("read"), None);
    write_raw(home.path(), "");
    let got = InstallPaths::resolve_with(no_env(), home.path(), &PathOverrides::default())
        .expect("resolve");
    assert_eq!(got.projects_root.source, PathSource::Default);
    assert_eq!(got.config_dir.source, PathSource::Default);
    assert_eq!(got.claude_config_dir, None);
}

/// What: a relative or `~` value under each file key is a `Relative` error
/// naming the key and the install-file source, never a default.
#[test]
fn relative_file_value_is_rejected() {
    for key in [
        "projects_root",
        "config_dir",
        "claude_config_dir",
        "codex_home",
    ] {
        for bad in ["relative/dir", "~/x"] {
            let home = tempfile::tempdir().expect("tempdir");
            write_raw(home.path(), &format!("{key} = \"{bad}\"\n"));
            let err = InstallPaths::resolve_with(no_env(), home.path(), &PathOverrides::default())
                .expect_err("relative value must fail");
            match err {
                InstallPathsError::Relative { key: k, origin, .. } => {
                    assert_eq!(k, key);
                    assert_eq!(origin, PathSource::InstallFile);
                }
                other => panic!("{key}={bad}: expected Relative, got {other:?}"),
            }
        }
    }
}

/// What: an empty file value is an `Empty` error, not the default.
#[test]
fn empty_file_value_is_rejected() {
    let home = tempfile::tempdir().expect("tempdir");
    write_raw(home.path(), "projects_root = \"\"\n");
    let err = InstallPaths::resolve_with(no_env(), home.path(), &PathOverrides::default())
        .expect_err("empty value must fail");
    assert!(
        matches!(
            err,
            InstallPathsError::Empty {
                key: "projects_root",
                origin: PathSource::InstallFile
            }
        ),
        "got {err:?}"
    );
}

/// What: a relative env value is rejected with the env source.
#[test]
fn relative_env_value_is_rejected() {
    let home = tempfile::tempdir().expect("tempdir");
    let env = env_of(&[("CODEX_HOME", "codex")]);
    let err = InstallPaths::resolve_with(env, home.path(), &PathOverrides::default())
        .expect_err("relative env must fail");
    assert!(
        matches!(
            err,
            InstallPathsError::Relative {
                key: "codex_home",
                origin: PathSource::Env,
                ..
            }
        ),
        "got {err:?}"
    );
}

/// What: a relative override is rejected with the flag source.
#[test]
fn relative_flag_value_is_rejected() {
    let home = tempfile::tempdir().expect("tempdir");
    let flags = PathOverrides {
        config_dir: Some("cfg".into()),
        ..PathOverrides::default()
    };
    let err = InstallPaths::resolve_with(no_env(), home.path(), &flags)
        .expect_err("relative flag must fail");
    assert!(
        matches!(
            err,
            InstallPathsError::Relative {
                key: "config_dir",
                origin: PathSource::Flag,
                ..
            }
        ),
        "got {err:?}"
    );
}

/// Why: the #11821 P2 critic MEDIUM: the workspace-root chain ranked the
/// YAML template above the install file while `InstallPaths::resolve` did not
/// read it at all, so one config gave two roots.
/// What: the one rule for `projects_root`: install file > template > default;
/// the template is `~`-expanded and labelled `Template`.
#[test]
fn file_beats_template_and_template_beats_default() {
    let home = tempfile::tempdir().expect("tempdir");
    let tpl = PathOverrides {
        projects_root_template: Some("  ~/tpl  ".into()),
        ..PathOverrides::default()
    };
    let got = InstallPaths::resolve_with(no_env(), home.path(), &tpl).expect("resolve");
    assert_eq!(got.projects_root.path, home.path().join("tpl"));
    assert_eq!(got.projects_root.source, PathSource::Template);
    assert_eq!(got.config_dir.source, PathSource::Default);

    write_raw(home.path(), "projects_root = \"/file/projects\"\n");
    let got = InstallPaths::resolve_with(no_env(), home.path(), &tpl).expect("resolve");
    assert_eq!(
        got.projects_root,
        rp("/file/projects", PathSource::InstallFile)
    );

    let blank = PathOverrides {
        projects_root_template: Some("   ".into()),
        ..PathOverrides::default()
    };
    write_raw(home.path(), "");
    let got = InstallPaths::resolve_with(no_env(), home.path(), &blank).expect("resolve");
    assert_eq!(got.projects_root.source, PathSource::Default);
}

/// What: a relative template is a `Relative` error with the template source.
#[test]
fn relative_template_value_is_rejected() {
    let home = tempfile::tempdir().expect("tempdir");
    let tpl = PathOverrides {
        projects_root_template: Some("projects".into()),
        ..PathOverrides::default()
    };
    let err = InstallPaths::resolve_with(no_env(), home.path(), &tpl)
        .expect_err("relative template must fail");
    assert!(
        matches!(
            err,
            InstallPathsError::Relative {
                key: "projects_root",
                origin: PathSource::Template,
                ..
            }
        ),
        "got {err:?}"
    );
}

/// Why: the #11821 P2 critic LOW: with no home, the `try_` projects-root path
/// answered `/tmp/trusty-mpm-projects`, a root nobody configured.
/// What: no home -> `NoHome`, even with an absolute template; the injected
/// core is the only way to reach it on hosts whose `dirs::home_dir()` falls
/// back to the password database.
#[test]
fn try_projects_root_fails_closed_without_a_home() {
    let got = super::process::projects_root_with(no_env(), None, Some("/abs/tpl"));
    assert!(
        matches!(got, Err(InstallPathsError::NoHome)),
        "no home must fail closed, got {got:?}"
    );
    let env = env_of(&[("TRUSTY_MPM_WORKSPACE_ROOT", "/env/projects")]);
    let got = super::process::projects_root_with(env, None, None);
    assert!(matches!(got, Err(InstallPathsError::NoHome)), "got {got:?}");
}

/// What: a misspelt key fails the parse instead of being ignored.
#[test]
fn unknown_key_is_rejected() {
    let home = tempfile::tempdir().expect("tempdir");
    write_raw(home.path(), "project_root = \"/x\"\n");
    let err = InstallPaths::resolve_with(no_env(), home.path(), &PathOverrides::default())
        .expect_err("unknown key must fail");
    match err {
        InstallPathsError::Parse { message, .. } => {
            assert!(message.contains("project_root"), "message: {message}");
        }
        other => panic!("expected Parse, got {other:?}"),
    }
}

/// What: the file is read from `<home>/.trusty-tools`, never from the
/// configured config dir, even when the file itself moves the config dir.
#[test]
fn install_file_is_anchored_at_home_not_config_dir() {
    let home = tempfile::tempdir().expect("tempdir");
    let cfg = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        install_file_path(home.path()),
        home.path().join(".trusty-tools").join("install.toml")
    );
    write_raw(
        home.path(),
        &format!("config_dir = \"{}\"\n", cfg.path().display()),
    );
    write_raw(cfg.path(), "projects_root = \"/from/config/dir\"\n");
    let got = InstallPaths::resolve_with(no_env(), home.path(), &PathOverrides::default())
        .expect("resolve");
    assert_eq!(got.config_dir.path, cfg.path());
    assert_eq!(got.projects_root.source, PathSource::Default);
}

/// What: `~` is expanded once at write time, the stored file holds absolute
/// paths only, and the resolver reads them back as install-file values.
#[test]
fn writer_round_trips_and_expands_tilde() {
    let home = tempfile::tempdir().expect("tempdir");
    let values = InstallFile {
        projects_root: Some("~/work".into()),
        config_dir: Some("/srv/cfg".into()),
        ..InstallFile::default()
    };
    let (path, stored) = write_install_file(home.path(), &values).expect("write");
    assert_eq!(path, install_file_path(home.path()));
    assert_eq!(stored.projects_root, Some(home.path().join("work")));
    let raw = std::fs::read_to_string(&path).expect("read back");
    assert!(!raw.contains('~'), "stored file must hold no `~`: {raw}");
    assert!(!raw.contains("codex_home"), "unset keys are omitted: {raw}");

    let got = InstallPaths::resolve_with(no_env(), home.path(), &PathOverrides::default())
        .expect("resolve");
    assert_eq!(got.projects_root.path, home.path().join("work"));
    assert_eq!(got.projects_root.source, PathSource::InstallFile);
    assert_eq!(got.config_dir, rp("/srv/cfg", PathSource::InstallFile));
}

/// What: a relative or empty value is refused and no file is written.
#[test]
fn writer_refuses_relative_paths() {
    let home = tempfile::tempdir().expect("tempdir");
    for (values, want_empty) in [
        (
            InstallFile {
                codex_home: Some("codex".into()),
                ..InstallFile::default()
            },
            false,
        ),
        (
            InstallFile {
                projects_root: Some("".into()),
                ..InstallFile::default()
            },
            true,
        ),
    ] {
        let err = write_install_file(home.path(), &values).expect_err("must refuse");
        if want_empty {
            assert!(
                matches!(err, InstallPathsError::Empty { .. }),
                "got {err:?}"
            );
        } else {
            assert!(
                matches!(err, InstallPathsError::Relative { .. }),
                "got {err:?}"
            );
        }
    }
    assert!(!install_file_path(home.path()).exists());
}

/// Why: P1's contract is that existing callers see no change with no new
/// env or file set; this pins each wrapper to its pre-#11821 formula.
/// What: with `$HOME` at a tempdir and the two env vars cleared, the wrappers
/// return exactly the old values; an `install.toml` then moves them (above
/// the YAML template, the one rule since #11821 P3). With a broken one, `crate_config_dir` fails closed
/// (`None`, #11821); `resolve_workspace_root` keeps its logged default
/// because its `PathBuf` signature cannot carry an error. Writers use
/// `try_resolve_workspace_root`, pinned by
/// `try_resolve_workspace_root_fails_closed_on_broken_install_file`.
#[test]
fn process_env_wrappers_match_today() {
    use crate::workspace_layout::resolve_workspace_root;

    // #11821 P3: sets HOME and the env vars under the crate-wide `ENV_LOCK` (#5937).
    let _guard = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().expect("tempdir");
    let h = home.path().to_path_buf();
    let real_home = std::env::var_os("HOME");
    let saved: Vec<_> = [WORKSPACE_ROOT_ENV, CONFIG_DIR_ENV]
        .into_iter()
        .map(|k| (k, std::env::var_os(k)))
        .collect();
    // SAFETY: guarded by `ENV_LOCK`; everything is restored below.
    unsafe {
        std::env::set_var("HOME", &h);
        std::env::remove_var(WORKSPACE_ROOT_ENV);
        std::env::remove_var(CONFIG_DIR_ENV);
    }

    let before_root = resolve_workspace_root(None);
    let before_template = resolve_workspace_root(Some("/tmp/from-config"));
    #[cfg(feature = "crate-config")]
    let before_cfg = (
        crate::crate_config::crate_config_dir("trusty-mpm"),
        crate::crate_config::crate_config_path("trusty-mpm"),
    );
    write_raw(
        &h,
        "projects_root = \"/file/projects\"\nconfig_dir = \"/file/cfg\"\n",
    );
    let file_root = resolve_workspace_root(None);
    let file_template = resolve_workspace_root(Some("/tmp/from-config"));
    #[cfg(feature = "crate-config")]
    let file_cfg = crate::crate_config::crate_config_dir("trusty-mpm");
    write_raw(&h, "bogus = 1\n");
    let broken_root = resolve_workspace_root(None);
    #[cfg(feature = "crate-config")]
    let broken_cfg = crate::crate_config::crate_config_dir("trusty-mpm");

    // SAFETY: as above.
    unsafe {
        match &real_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        for (k, v) in &saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    // Pre-#11821 formulas: `home.join(DEFAULT_WORKSPACE_DIR)` and
    // `crate_config_dir_at(&home, crate)`.
    assert_eq!(before_root, h.join(DEFAULT_WORKSPACE_DIR));
    assert_eq!(before_template, PathBuf::from("/tmp/from-config"));
    assert_eq!(file_root, PathBuf::from("/file/projects"));
    assert_eq!(file_template, PathBuf::from("/file/projects"));
    assert_eq!(broken_root, h.join(DEFAULT_WORKSPACE_DIR));
    #[cfg(feature = "crate-config")]
    {
        use crate::crate_config::{crate_config_dir_at, crate_config_path_at};
        assert_eq!(before_cfg.0, Some(crate_config_dir_at(&h, "trusty-mpm")));
        assert_eq!(before_cfg.1, Some(crate_config_path_at(&h, "trusty-mpm")));
        assert_eq!(
            file_cfg,
            Some(PathBuf::from("/file/cfg/.trusty-tools/trusty-mpm"))
        );
        assert_eq!(
            broken_cfg, None,
            "a broken install.toml must not yield $HOME"
        );
    }
}

/// Run `f` with `$HOME` at `home` and the config-dir env var set to
/// `config_dir_env` (or cleared), under `ENV_LOCK`, restoring both after.
#[cfg(feature = "crate-config")]
fn with_process_env<R>(home: &Path, config_dir_env: Option<&Path>, f: impl FnOnce() -> R) -> R {
    let _guard = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let saved_home = std::env::var_os("HOME");
    let saved_cfg = std::env::var_os(CONFIG_DIR_ENV);
    // SAFETY: guarded by `ENV_LOCK`; restored below before returning.
    unsafe {
        std::env::set_var("HOME", home);
        match config_dir_env {
            Some(v) => std::env::set_var(CONFIG_DIR_ENV, v),
            None => std::env::remove_var(CONFIG_DIR_ENV),
        }
    }
    let out = f();
    // SAFETY: as above.
    unsafe {
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match saved_cfg {
            Some(v) => std::env::set_var(CONFIG_DIR_ENV, v),
            None => std::env::remove_var(CONFIG_DIR_ENV),
        }
    }
    out
}

/// Why: a valid env value outranks the file, so a broken file must not block it.
/// What: every key set by env + a malformed install.toml -> all env, no error;
/// the process-env wrapper agrees for `TRUSTY_MPM_CONFIG_DIR`.
#[test]
fn env_wins_over_broken_file() {
    let home = tempfile::tempdir().expect("tempdir");
    write_raw(home.path(), "bogus = 1\n");
    let got = InstallPaths::resolve_with(env_of(&FULL_ENV), home.path(), &PathOverrides::default())
        .expect("env must win without parsing the broken file");
    assert_eq!(got.projects_root, rp("/env/projects", PathSource::Env));
    assert_eq!(got.config_dir, rp("/env/config", PathSource::Env));
    assert_eq!(got.codex_home, Some(rp("/env/codex", PathSource::Env)));

    #[cfg(feature = "crate-config")]
    {
        let cfg = tempfile::tempdir().expect("tempdir");
        let dir = with_process_env(home.path(), Some(cfg.path()), || {
            crate::crate_config::crate_config_dir("tc-11821")
        });
        assert_eq!(dir, Some(cfg.path().join(".trusty-tools").join("tc-11821")));
    }
}

/// Why: the #11821 critic HIGH — on 8ae77e5fb5 a broken install.toml made
/// `crate_config::save` write under `$HOME/.trusty-tools/<crate>` instead of
/// failing.
/// What: with a malformed install.toml and no env override, `save` returns a
/// typed `InvalidData` I/O error naming the install file, `load` errors too,
/// and nothing is written under `$HOME/.trusty-tools/<crate>`.
#[cfg(feature = "crate-config")]
#[test]
fn save_fails_closed_on_broken_install_file() {
    use std::collections::BTreeMap;

    let home = tempfile::tempdir().expect("tempdir");
    write_raw(home.path(), "bogus = 1\n");
    let value: BTreeMap<String, String> = [("k".to_string(), "v".to_string())].into();
    let (saved, loaded, dir) = with_process_env(home.path(), None, || {
        (
            crate::crate_config::save("tc-11821", &value),
            crate::crate_config::load::<BTreeMap<String, String>>("tc-11821"),
            crate::crate_config::crate_config_dir("tc-11821"),
        )
    });
    match saved {
        Err(crate::crate_config::ConfigError::Io { path, source }) => {
            assert_eq!(path, install_file_path(home.path()));
            assert_eq!(source.kind(), std::io::ErrorKind::InvalidData);
        }
        other => panic!("save must fail closed, got {other:?}"),
    }
    assert!(
        loaded.is_err(),
        "load must surface the broken file: {loaded:?}"
    );
    assert_eq!(dir, None);
    assert!(
        !home.path().join(".trusty-tools").join("tc-11821").exists(),
        "nothing may be written under $HOME"
    );
}

/// What: a parse error names the line and column.
#[test]
fn parse_error_names_line_and_column() {
    let home = tempfile::tempdir().expect("tempdir");
    write_raw(home.path(), "projects_root = \"/a\"\nbogus = 1\n");
    match read_install_file(home.path()) {
        Err(InstallPathsError::Parse { message, .. }) => {
            assert!(message.contains("line 2, column 1"), "message: {message}");
        }
        other => panic!("expected Parse, got {other:?}"),
    }
}

/// What: the source labels `tm paths show` will print.
#[test]
fn source_labels() {
    let labels: Vec<String> = [
        PathSource::Flag,
        PathSource::Env,
        PathSource::InstallFile,
        PathSource::Template,
        PathSource::Default,
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
        labels,
        ["flag", "env", "install-file", "config-template", "default"]
    );
}

/// Why: df-D1t (#11821): a writer must not act on the default projects root
/// while `install.toml` is broken.
/// What: with `$HOME` at a tempdir and `TRUSTY_MPM_WORKSPACE_ROOT` cleared,
/// `try_resolve_workspace_root` returns the default with no file, the file's
/// value with a valid one, and `Parse` with a broken one, while the legacy
/// wrapper still answers the default. The env var wins without reading the
/// broken file; a template sits below the file, so it fails closed too.
#[test]
fn try_resolve_workspace_root_fails_closed_on_broken_install_file() {
    use crate::workspace_layout::{resolve_workspace_root, try_resolve_workspace_root};

    // #11821 P3: sets HOME and the env var under the crate-wide `ENV_LOCK` (#5937).
    let _guard = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().expect("tempdir");
    let h = home.path().to_path_buf();
    let saved_home = std::env::var_os("HOME");
    let saved_root = std::env::var_os(WORKSPACE_ROOT_ENV);
    // SAFETY: guarded by `ENV_LOCK`; everything is restored below.
    unsafe {
        std::env::set_var("HOME", &h);
        std::env::remove_var(WORKSPACE_ROOT_ENV);
    }

    let absent = try_resolve_workspace_root(None);
    write_raw(&h, "projects_root = \"/file/projects\"\n");
    let valid = try_resolve_workspace_root(None);
    write_raw(&h, "projects_root = \"/file/projects\"\nbogus = 1\n");
    let broken = try_resolve_workspace_root(None);
    let broken_legacy = resolve_workspace_root(None);
    let broken_template = try_resolve_workspace_root(Some("/tmp/from-config"));
    // SAFETY: as above.
    unsafe { std::env::set_var(WORKSPACE_ROOT_ENV, "/env/projects") };
    let broken_env = try_resolve_workspace_root(None);

    // SAFETY: as above.
    unsafe {
        match &saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match &saved_root {
            Some(v) => std::env::set_var(WORKSPACE_ROOT_ENV, v),
            None => std::env::remove_var(WORKSPACE_ROOT_ENV),
        }
    }

    assert_eq!(absent.expect("no file"), h.join(DEFAULT_WORKSPACE_DIR));
    assert_eq!(valid.expect("valid file"), PathBuf::from("/file/projects"));
    assert!(
        matches!(broken, Err(InstallPathsError::Parse { .. })),
        "a broken install.toml must fail closed, got {broken:?}"
    );
    assert_eq!(broken_legacy, h.join(DEFAULT_WORKSPACE_DIR));
    assert!(
        matches!(broken_template, Err(InstallPathsError::Parse { .. })),
        "the template is below the install file, got {broken_template:?}"
    );
    assert_eq!(
        broken_env.expect("env wins"),
        PathBuf::from("/env/projects")
    );
}

/// Run `f` with `$HOME` at `home` and `TRUSTY_MPM_WORKSPACE_ROOT` set to
/// `root_env` (or cleared), under `ENV_LOCK`, restoring both after.
fn with_root_env<R>(home: &Path, root_env: Option<&str>, f: impl FnOnce() -> R) -> R {
    let _guard = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let saved_home = std::env::var_os("HOME");
    let saved_root = std::env::var_os(WORKSPACE_ROOT_ENV);
    // SAFETY: guarded by `ENV_LOCK`; restored below before returning.
    unsafe {
        std::env::set_var("HOME", home);
        match root_env {
            Some(v) => std::env::set_var(WORKSPACE_ROOT_ENV, v),
            None => std::env::remove_var(WORKSPACE_ROOT_ENV),
        }
    }
    let out = f();
    // SAFETY: as above.
    unsafe {
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match saved_root {
            Some(v) => std::env::set_var(WORKSPACE_ROOT_ENV, v),
            None => std::env::remove_var(WORKSPACE_ROOT_ENV),
        }
    }
    out
}

/// Why: the #11821 P2 critic MEDIUM: the writer chain accepted a relative env
/// or template root that `InstallPaths::resolve` refuses.
/// What: a relative `TRUSTY_MPM_WORKSPACE_ROOT` and a relative template are
/// each a `Relative` error from `try_resolve_workspace_root`, naming the
/// source; the reader wrapper logs and answers the default; a valid
/// install file outranks the template, as in `InstallPaths::resolve`.
#[test]
fn try_resolve_workspace_root_refuses_relative_env_and_template() {
    use crate::workspace_layout::{resolve_workspace_root, try_resolve_workspace_root};

    let home = tempfile::tempdir().expect("tempdir");
    let h = home.path().to_path_buf();
    let (env_try, env_legacy) = with_root_env(&h, Some("rel/env"), || {
        (
            try_resolve_workspace_root(None),
            resolve_workspace_root(None),
        )
    });
    let tpl_try = with_root_env(&h, None, || try_resolve_workspace_root(Some("rel/tpl")));
    write_raw(&h, "projects_root = \"/file/projects\"\n");
    let (ranked, agreed) = with_root_env(&h, None, || {
        (
            try_resolve_workspace_root(Some("/tmp/from-config")),
            InstallPaths::resolve_with(
                |n| std::env::var(n).ok(),
                &h,
                &PathOverrides {
                    projects_root_template: Some("/tmp/from-config".into()),
                    ..PathOverrides::default()
                },
            ),
        )
    });

    assert!(
        matches!(
            env_try,
            Err(InstallPathsError::Relative {
                key: "projects_root",
                origin: PathSource::Env,
                ..
            })
        ),
        "a relative env root must fail closed, got {env_try:?}"
    );
    assert_eq!(env_legacy, h.join(DEFAULT_WORKSPACE_DIR));
    assert!(
        matches!(
            tpl_try,
            Err(InstallPathsError::Relative {
                origin: PathSource::Template,
                ..
            })
        ),
        "a relative template must fail closed, got {tpl_try:?}"
    );
    let ranked = ranked.expect("valid file");
    assert_eq!(ranked, PathBuf::from("/file/projects"));
    assert_eq!(agreed.expect("resolve").projects_root.path, ranked);
}

/// Why: trusty-mpm's framework layout resolves the config dir through
/// `try_config_dir` (#11821 P3).
/// What: no env, no file -> `$HOME`; the file's `config_dir` next; the env
/// var over the file; a broken file -> `Parse`, never `$HOME`.
#[cfg(feature = "crate-config")]
#[test]
fn try_config_dir_follows_env_file_and_fails_closed() {
    let home = tempfile::tempdir().expect("tempdir");
    let h = home.path();
    let absent = with_process_env(h, None, try_config_dir);
    write_raw(h, "config_dir = \"/file/cfg\"\n");
    let file = with_process_env(h, None, try_config_dir);
    let env = with_process_env(h, Some(Path::new("/env/cfg")), try_config_dir);
    write_raw(h, "config_dir = \"/file/cfg\"\nbogus = 1\n");
    let broken = with_process_env(h, None, try_config_dir);

    assert_eq!(absent.expect("default"), h);
    assert_eq!(file.expect("file"), PathBuf::from("/file/cfg"));
    assert_eq!(env.expect("env"), PathBuf::from("/env/cfg"));
    assert!(
        matches!(broken, Err(InstallPathsError::Parse { .. })),
        "a broken install.toml must fail closed, got {broken:?}"
    );
}

/// What: an explicit home's own install file decides its config dir; a
/// broken one is `Parse`, never that home.
#[cfg(feature = "crate-config")]
#[test]
fn try_config_dir_under_reads_the_install_file_of_that_home() {
    let process_home = tempfile::tempdir().expect("tempdir");
    let other = tempfile::tempdir().expect("tempdir");
    write_raw(other.path(), "config_dir = \"/other/cfg\"\n");
    let got = with_process_env(process_home.path(), None, || {
        try_config_dir_under(other.path())
    });
    assert_eq!(got.expect("file"), PathBuf::from("/other/cfg"));
    let plain = with_process_env(process_home.path(), None, || {
        try_config_dir_under(process_home.path())
    });
    assert_eq!(plain.expect("default"), process_home.path());
    let env = Some(Path::new("/env/cfg"));
    let (own, scratch) = with_process_env(process_home.path(), env, || {
        (
            try_config_dir_under(process_home.path()),
            try_config_dir_under(other.path()),
        )
    });
    assert_eq!(own.expect("env"), PathBuf::from("/env/cfg"));
    assert_eq!(
        scratch.expect("file, env ignored"),
        PathBuf::from("/other/cfg")
    );
    write_raw(other.path(), "bogus = 1\n");
    let broken = with_process_env(process_home.path(), None, || {
        try_config_dir_under(other.path())
    });
    assert!(
        matches!(broken, Err(InstallPathsError::Parse { .. })),
        "got {broken:?}"
    );
}
