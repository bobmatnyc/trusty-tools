//! #9224: tagent's own `.env` and self-project `.env.local` loads honour
//! `TRUSTY_SANDBOX=1`. Every test drives the hermetic core with explicit
//! paths and flag values; none touches process env or the real cwd.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::load_startup_env_tiers;

const DOTENV_KEY: &str = "TAGENT_9224_CWD_DOTENV_CANARY";
const DOTENV_VALUE: &str = "canary-9224-cwd-dotenv";
const PROJECT_KEY: &str = "TAGENT_9224_SELF_PROJECT_CANARY";
const PROJECT_VALUE: &str = "canary-9224-self-project";

/// A tree with a `.env` above a nested cwd and a separate self-project dir
/// holding `.env.local`, each binding one canary.
struct Fixture {
    _root: tempfile::TempDir,
    cwd: PathBuf,
    project: PathBuf,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("tempdir");
    let workspace = root.path().join("workspace");
    let cwd = workspace.join("nested").join("deeper");
    let project = root.path().join("self-project");
    std::fs::create_dir_all(&cwd).expect("create cwd");
    std::fs::create_dir_all(&project).expect("create project");
    std::fs::write(
        workspace.join(".env"),
        format!("{DOTENV_KEY}={DOTENV_VALUE}\n"),
    )
    .expect("write .env");
    std::fs::write(
        project.join(".env.local"),
        format!("{PROJECT_KEY}={PROJECT_VALUE}\n"),
    )
    .expect("write .env.local");
    Fixture {
        _root: root,
        cwd,
        project,
    }
}

/// Run the core and collect what each `load` call would have set, parsing
/// with dotenvy's non-mutating iterator instead of touching process env.
fn loaded_vars(fx: &Fixture, sandbox_value: Option<&OsStr>) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    load_startup_env_tiers(
        sandbox_value,
        Some(fx.cwd.as_path()),
        Some(fx.project.as_path()),
        |path: &Path| {
            for (key, value) in dotenvy::from_path_iter(path)
                .expect("load target must be readable")
                .flatten()
            {
                vars.entry(key).or_insert(value);
            }
        },
    );
    vars
}

fn has(vars: &HashMap<String, String>, key: &str, value: &str) -> bool {
    vars.get(key).is_some_and(|v| v == value)
}

#[test]
fn sandbox_flag_skips_self_project_env_local() {
    let fx = fixture();
    let vars = loaded_vars(&fx, Some(OsStr::new("1")));
    assert!(
        !vars.contains_key(PROJECT_KEY),
        "TRUSTY_SANDBOX=1 must not load the self-project .env.local"
    );
}

#[test]
fn sandbox_flag_skips_cwd_dotenv() {
    let fx = fixture();
    let vars = loaded_vars(&fx, Some(OsStr::new("1")));
    assert!(
        !vars.contains_key(DOTENV_KEY),
        "TRUSTY_SANDBOX=1 must not load the cwd .env"
    );
}

#[test]
fn flag_off_loads_both_tiers() {
    let fx = fixture();
    let vars = loaded_vars(&fx, None);
    assert!(
        has(&vars, PROJECT_KEY, PROJECT_VALUE),
        "with TRUSTY_SANDBOX unset the self-project .env.local must load"
    );
    assert!(
        has(&vars, DOTENV_KEY, DOTENV_VALUE),
        "with TRUSTY_SANDBOX unset the .env above the cwd must load"
    );
}

#[test]
fn values_other_than_one_do_not_opt_out() {
    let fx = fixture();
    for value in ["", "0", "true", " 1", "1 ", "yes"] {
        let vars = loaded_vars(&fx, Some(OsStr::new(value)));
        assert!(
            has(&vars, PROJECT_KEY, PROJECT_VALUE),
            "TRUSTY_SANDBOX={value:?} must not opt out of the self-project .env.local"
        );
        assert!(
            has(&vars, DOTENV_KEY, DOTENV_VALUE),
            "TRUSTY_SANDBOX={value:?} must not opt out of the cwd .env"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let vars = loaded_vars(&fx, Some(OsStr::from_bytes(b"1\xff")));
        assert!(
            has(&vars, PROJECT_KEY, PROJECT_VALUE),
            "a non-UTF-8 TRUSTY_SANDBOX must not opt out of the self-project .env.local"
        );
        assert!(
            has(&vars, DOTENV_KEY, DOTENV_VALUE),
            "a non-UTF-8 TRUSTY_SANDBOX must not opt out of the cwd .env"
        );
    }
}
