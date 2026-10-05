//! #9224: tagent's own `.env` and self-project `.env.local` loads honour
//! `TRUSTY_SANDBOX=1`. Every test drives the hermetic core with explicit
//! paths and flag values; none touches process env or the real cwd.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::{find_dotenv_upward, load_startup_env_tiers};

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
        || Some(fx.project.clone()),
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

/// One step of the core's run, in the order it happened.
#[derive(Debug, PartialEq)]
enum Step {
    Load(PathBuf),
    Resolve,
}

/// #9224: the resolver reads the `TAGENT_PROJECT_DIR` hint from process env,
/// which the cwd `.env` may set, so it must run after that load.
#[test]
fn self_project_resolves_after_cwd_dotenv_load() {
    let fx = fixture();
    let steps = RefCell::new(Vec::new());
    load_startup_env_tiers(
        None,
        Some(fx.cwd.as_path()),
        || {
            steps.borrow_mut().push(Step::Resolve);
            Some(fx.project.clone())
        },
        |path: &Path| steps.borrow_mut().push(Step::Load(path.to_path_buf())),
    );
    let dotenv = fx.cwd.ancestors().nth(2).expect("workspace").join(".env");
    assert_eq!(
        steps.into_inner(),
        vec![
            Step::Load(dotenv),
            Step::Resolve,
            Step::Load(fx.project.join(".env.local")),
        ],
        "the self-project must resolve after the cwd .env load"
    );
}

/// #9224: under the opt-out the self-project is never even located.
#[test]
fn sandbox_flag_never_calls_the_resolver() {
    let fx = fixture();
    let called = Cell::new(false);
    let mut loads = 0;
    load_startup_env_tiers(
        Some(OsStr::new("1")),
        Some(fx.cwd.as_path()),
        || {
            called.set(true);
            Some(fx.project.clone())
        },
        |_: &Path| loads += 1,
    );
    assert!(!called.get(), "TRUSTY_SANDBOX=1 must not call the resolver");
    assert_eq!(loads, 0, "TRUSTY_SANDBOX=1 must load nothing");
}

/// What each fixture path is.
#[derive(Clone, Copy)]
enum Entry {
    File,
    Dir,
}

/// One `find_dotenv_upward` case; paths are relative to a fresh temp root.
struct FinderCase {
    name: &'static str,
    entries: &'static [(&'static str, Entry)],
    start: &'static str,
    /// The directory whose `.env` must be found.
    expected: &'static str,
}

/// #9224: the nearest `.env` regular file wins; a directory named `.env` is
/// skipped and the search continues upward.
#[test]
fn find_dotenv_upward_picks_the_nearest_regular_file() {
    let table = [
        FinderCase {
            name: "nested ancestors: the nearer .env wins",
            entries: &[("a/.env", Entry::File), ("a/b/.env", Entry::File)],
            start: "a/b/c",
            expected: "a/b",
        },
        FinderCase {
            name: "a directory named .env is skipped",
            entries: &[("a/.env", Entry::File), ("a/b/.env", Entry::Dir)],
            start: "a/b/c",
            expected: "a",
        },
        FinderCase {
            name: "the start dir itself is checked first",
            entries: &[("a/.env", Entry::File), ("a/b/c/.env", Entry::File)],
            start: "a/b/c",
            expected: "a/b/c",
        },
    ];
    for case in table {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(root.path().join(case.start)).expect("create start");
        for (rel, kind) in case.entries {
            let path = root.path().join(rel);
            match kind {
                Entry::File => std::fs::write(&path, "K=v\n").expect("write .env"),
                Entry::Dir => std::fs::create_dir_all(&path).expect("create .env dir"),
            }
        }
        assert_eq!(
            find_dotenv_upward(&root.path().join(case.start)),
            Some(root.path().join(case.expected).join(".env")),
            "{}",
            case.name
        );
    }
}

/// A metadata error other than `NotFound` ends the search with `None`, even
/// with a real `.env` further up.
#[cfg(unix)]
#[test]
fn find_dotenv_upward_stops_at_an_unreadable_ancestor() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().expect("tempdir");
    let locked = root.path().join("locked");
    let start = locked.join("inner");
    std::fs::create_dir_all(&start).expect("create start");
    std::fs::write(root.path().join(".env"), "K=v\n").expect("write .env");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("lock dir");
    // Root enters any directory; then there is no error to observe.
    let enforced = std::fs::metadata(&start).is_err();
    let found = find_dotenv_upward(&start);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("unlock dir");
    if enforced {
        assert_eq!(found, None, "an unreadable ancestor must stop the search");
    }
}
