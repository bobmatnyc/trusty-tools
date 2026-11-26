//! #11821 P4-f2: `tm architect poll`'s config reads the claude-config dir
//! under the install's config dir, and fails closed on a broken install file.
//!
//! Why: before P4-f2 the poller joined `$HOME/.trusty-tools/...` whatever
//! `install.toml` or `TRUSTY_MPM_CONFIG_DIR` said, so with a moved config dir
//! it watched a directory tm no longer writes to.
//! What: drives [`verbs::poll_config`] over a scratch home: the install file's
//! `config_dir` and the env var each move the claude-config paths; a broken,
//! unreadable or relative install file is an error, not a `$HOME` fallback.
//! Test: the four tests below; each is red against the pre-fix crate.

use std::fs;
use std::path::{Path, PathBuf};

use trusty_architect::verbs;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// `<home>/.trusty-tools/install.toml`, named by hand so this test also
/// builds against the pre-fix crate.
fn install_file(home: &Path) -> PathBuf {
    home.join(".trusty-tools/install.toml") // path-literal: allow (fixture: the install file's fixed anchor, named by hand so the test runs pre-fix)
}

/// A scratch home whose install file holds `body`; returns (home, root).
fn home_with(body: &str) -> Result<(tempfile::TempDir, PathBuf), std::io::Error> {
    let home = tempfile::tempdir()?;
    let file = install_file(home.path());
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(file, body)?;
    let root = home.path().join("arch");
    Ok((home, root))
}

fn claude_config(base: &Path) -> PathBuf {
    base.join(".trusty-tools/trusty-mpm/claude-config")
}

#[test]
fn poll_config_follows_install_toml_config_dir() -> TestResult {
    let moved = tempfile::tempdir()?;
    let body = format!("config_dir = {:?}\n", moved.path().display().to_string());
    let (home, root) = home_with(&body)?;
    let c = verbs::poll_config(&root, home.path(), &|_| None)?;
    let want = claude_config(moved.path());
    assert_eq!(c.sessions_dir, want.join("sessions"));
    assert_eq!(c.transcript_base, want.join("projects"));
    assert_eq!(c.usage.claude_json, want.join(".claude.json"));
    Ok(())
}

#[test]
fn poll_config_env_config_dir_beats_a_broken_install_toml() -> TestResult {
    let (home, root) = home_with("config_dir = \"/never\nnot toml\n")?;
    let get = |k: &str| (k == "TRUSTY_MPM_CONFIG_DIR").then(|| "/moved/by/env".to_owned());
    let c = verbs::poll_config(&root, home.path(), &get)?;
    assert_eq!(
        c.sessions_dir,
        claude_config(Path::new("/moved/by/env")).join("sessions")
    );
    Ok(())
}

#[test]
fn poll_config_fails_closed_on_a_broken_install_toml() -> TestResult {
    let (home, root) = home_with("config_dir = \"/never\nnot toml\n")?;
    let err = verbs::poll_config(&root, home.path(), &|_| None)
        .expect_err("a broken install.toml must not fall back to $HOME");
    assert!(err.to_string().contains("install.toml"), "{err}");

    fs::remove_file(install_file(home.path()))?;
    fs::create_dir(install_file(home.path()))?;
    let err = verbs::poll_config(&root, home.path(), &|_| None)
        .expect_err("an unreadable install.toml must not fall back to $HOME");
    assert!(err.to_string().contains("cannot read"), "{err}");
    Ok(())
}

#[test]
fn poll_config_refuses_a_relative_config_dir() -> TestResult {
    let (home, root) = home_with("config_dir = \"relative/dir\"\n")?;
    let err = verbs::poll_config(&root, home.path(), &|_| None)
        .expect_err("a relative config_dir must be refused");
    assert!(err.to_string().contains("absolute"), "{err}");
    let get = |k: &str| (k == "TRUSTY_MPM_CONFIG_DIR").then(|| "rel".to_owned());
    let (bare, root) = home_with("")?;
    let err = verbs::poll_config(&root, bare.path(), &get)
        .expect_err("a relative TRUSTY_MPM_CONFIG_DIR must be refused");
    assert!(err.to_string().contains("absolute"), "{err}");
    Ok(())
}
