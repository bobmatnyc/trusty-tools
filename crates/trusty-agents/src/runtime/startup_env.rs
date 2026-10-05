//! tagent's startup credential-file loads: trusty-common's shared `.env.local`
//! tiers, then tagent's own `.env` and self-project `.env.local` (#9224).

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use trusty_common::credentials::{SANDBOX_ENV_VAR, load_env_local_once, sandbox_flag_set};

/// Load every credential file tagent reads at startup, unless sandboxed.
///
/// Why (#250, #2405, #9224): `.env.local` lookup is relative to cwd, so tagent
/// also loads the self-project `.env.local` found from its own executable,
/// plus the cwd `.env`. Those two loads bypassed `load_env_local_once`, so a
/// run under `TRUSTY_SANDBOX=1` (set by a sandboxed tm daemon and inherited by
/// its sessions) still loaded developer credentials.
/// What: runs [`load_env_local_once`] (which applies the opt-out itself), then
/// [`load_startup_env_tiers`] with the process value of
/// [`SANDBOX_ENV_VAR`], the cwd and [`crate::ctrl::detect_self_project`] as
/// the lazy self-project resolver. Each found file goes through
/// `dotenvy::from_path`, which never overrides an already-set variable; a
/// malformed file is skipped, as before.
/// Test: the decision through [`load_startup_env_tiers`]
/// (`startup_env_tests::sandbox_flag_skips_self_project_env_local`,
/// `startup_env_tests::sandbox_flag_skips_cwd_dotenv`,
/// `startup_env_tests::flag_off_loads_both_tiers`,
/// `startup_env_tests::values_other_than_one_do_not_opt_out`,
/// `startup_env_tests::self_project_resolves_after_cwd_dotenv_load`).
pub(super) fn load_startup_env_files() {
    load_env_local_once();
    // #9224: read the opt-out once; `var_os` has no error branch.
    let sandbox_value = std::env::var_os(SANDBOX_ENV_VAR);
    let cwd = std::env::current_dir().ok();
    // #9224: pass the resolver, not its result, so a `TAGENT_PROJECT_DIR`
    // hint set by the cwd `.env` still picks the self-project.
    load_startup_env_tiers(
        sandbox_value.as_deref(),
        cwd.as_deref(),
        crate::ctrl::detect_self_project,
        |path| {
            let _ = dotenvy::from_path(path);
        },
    );
}

/// Hermetic core of [`load_startup_env_files`]: hand each tagent-owned
/// credential file to `load`, or none when sandboxed.
///
/// Why (#9224): the flag value, cwd and self-project resolver are
/// parameters, so a test drives the opt-out and the load order with no
/// process-env mutation and no real cwd.
/// What: `sandbox_value` exactly `1` (per [`sandbox_flag_set`]) returns before
/// either file is located, and `self_project` is never called. Otherwise
/// calls `load` with the first `.env` found walking up from `cwd` (the
/// `dotenvy::dotenv` search), then calls `self_project` and loads its
/// `.env.local` when that is a file. The resolver runs after the `.env` load
/// because it reads the `TAGENT_PROJECT_DIR` hint from process env, which
/// that `.env` may set; this is the order startup used before #9224.
/// Test: `startup_env_tests::sandbox_flag_skips_self_project_env_local`,
/// `startup_env_tests::sandbox_flag_skips_cwd_dotenv`,
/// `startup_env_tests::flag_off_loads_both_tiers`,
/// `startup_env_tests::values_other_than_one_do_not_opt_out`,
/// `startup_env_tests::self_project_resolves_after_cwd_dotenv_load`,
/// `startup_env_tests::sandbox_flag_never_calls_the_resolver`.
pub(super) fn load_startup_env_tiers(
    sandbox_value: Option<&OsStr>,
    cwd: Option<&Path>,
    self_project: impl FnOnce() -> Option<PathBuf>,
    mut load: impl FnMut(&Path),
) {
    // #9224: a sandboxed tagent loads neither its `.env` nor the self-project
    // `.env.local`; same exactly-"1" rule as trusty-common's loader.
    if sandbox_flag_set(sandbox_value) {
        return;
    }
    if let Some(dotenv) = cwd.and_then(find_dotenv_upward) {
        load(&dotenv);
    }
    // #9224: resolve only now, after the `.env` load that may set the hint.
    if let Some(project_env) = self_project()
        .map(|dir| dir.join(".env.local"))
        .filter(|path| path.is_file())
    {
        load(&project_env);
    }
}

/// The `.env` file `dotenvy::dotenv` would load from `start`, or `None`.
///
/// Why: dotenvy keeps its finder private, and the hermetic core must search
/// from a cwd parameter rather than the process cwd.
/// What: checks `dir/.env` for `start` and each ancestor, returning the first
/// regular file. A metadata error other than `NotFound` ends the search with
/// `None`, as dotenvy's finder does, so an unreadable directory loads nothing.
/// Test: `startup_env_tests::find_dotenv_upward_picks_the_nearest_regular_file`,
/// `startup_env_tests::find_dotenv_upward_stops_at_an_unreadable_ancestor`.
fn find_dotenv_upward(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let candidate = dir.join(".env");
        match std::fs::metadata(&candidate) {
            Ok(meta) if meta.is_file() => return Some(candidate),
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    None
}

#[cfg(test)]
#[path = "startup_env_tests.rs"]
mod startup_env_tests;
