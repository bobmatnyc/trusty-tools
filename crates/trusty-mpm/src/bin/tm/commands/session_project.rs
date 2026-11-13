//! A session start registers its checkout as a project (#10368).
//!
//! Why: `tm connect` wrote only `project-paths.json`, so the checkout it
//! started a session in was never a `projects.json` project and
//! `tm secrets assign` could not name it.
//! What: [`register_at_session_start`] runs [`register_session_checkout`]
//! against the registry under the caller's home; a failure is printed, never
//! read as a registration.
//! Test: `a_session_started_checkout_is_an_assign_target` and
//! `a_failed_session_start_registration_leaves_assign_refusing` in
//! `secrets/assign_tests.rs`.

use std::path::{Path, PathBuf};

use trusty_mpm::project::register_session_checkout;

/// The registry directory of the framework root under `home`.
///
/// What: `<home>/.trusty-mpm/project-registry`, the directory
/// `registry_data_dir()` names when `home` is the process home.
pub(crate) fn registry_dir_under_home(home: &Path) -> PathBuf {
    let root = trusty_mpm::core::paths::FrameworkPaths::under(home).root;
    trusty_mpm::project::registry_data_dir_under(&root)
}

/// Register the checkout holding `dir` as a project, at session start.
///
/// Why: #10368 — the checkout a session starts in must be a registered
/// target for `tm secrets` with no manual step. D50: only the session's own
/// checkout root, never home or above it.
/// What: [`register_session_checkout`] against [`registry_dir_under_home`];
/// no home registers nothing. Every failure prints [`project_notice`]'s
/// warning to stderr. The session still starts, and the registry `assign`
/// reads holds no row for it.
/// Test: `a_session_started_checkout_is_an_assign_target`.
pub(crate) async fn register_at_session_start(dir: &Path, home: Option<&Path>) {
    if let Some(line) = project_notice(dir, home).await {
        eprintln!("{line}");
    }
}

/// [`register_at_session_start`]'s work: the warning to print, or `None`.
///
/// Test: `a_session_started_checkout_is_an_assign_target`,
/// `a_failed_session_start_registration_leaves_assign_refusing`.
pub(crate) async fn project_notice(dir: &Path, home: Option<&Path>) -> Option<String> {
    let Some(home) = home else {
        return Some(failure_notice("no home directory"));
    };
    let result = register_session_checkout(&registry_dir_under_home(home), dir, Some(home)).await;
    // #10368: a failure is reported, never taken as registered.
    result.err().map(|e| failure_notice(&e.to_string()))
}

/// The warning a failed registration prints.
fn failure_notice(cause: &str) -> String {
    format!(
        "warning: this checkout was not registered as a project ({cause}); \
         `tm secrets assign` will refuse it until it is (#10368)"
    )
}
