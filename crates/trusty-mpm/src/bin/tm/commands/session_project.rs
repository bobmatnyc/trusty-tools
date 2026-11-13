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

use trusty_mpm::project::{SessionRegistration, register_session_checkout};

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

/// [`register_at_session_start`]'s work: the line to print, or `None`.
///
/// What: a failure's warning; for a row kept for another checkout, a note
/// naming both paths; otherwise `None`.
/// Test: `a_session_started_checkout_is_an_assign_target`,
/// `a_failed_session_start_registration_leaves_assign_refusing`,
/// `a_session_start_reports_a_project_registered_to_another_clone`.
pub(crate) async fn project_notice(dir: &Path, home: Option<&Path>) -> Option<String> {
    let Some(home) = home else {
        return Some(failure_notice("no home directory"));
    };
    let result = register_session_checkout(&registry_dir_under_home(home), dir, Some(home)).await;
    match result {
        // #10368: a row for another clone is reported, never rewritten.
        Ok(SessionRegistration::AlreadyRegistered {
            name,
            checkout,
            root,
        }) if checkout.as_ref() != Some(&root) => {
            Some(stale_checkout_notice(&name, checkout.as_deref(), &root))
        }
        Ok(_) => None,
        // #10368: a failure is reported, never taken as registered.
        Err(e) => Some(failure_notice(&e.to_string())),
    }
}

/// The note for a project whose registered checkout is not this session's.
fn stale_checkout_notice(name: &str, checkout: Option<&Path>, root: &Path) -> String {
    let registered = checkout.map_or_else(|| "none".to_string(), |c| c.display().to_string());
    format!(
        "note: project {name} is registered with checkout {registered}, not this session's \
         checkout {}; the registry was not changed, and `tm secrets assign` uses the \
         registered one (#10368)",
        root.display()
    )
}

/// The warning a failed registration prints.
fn failure_notice(cause: &str) -> String {
    format!(
        "warning: this checkout was not registered as a project ({cause}); \
         `tm secrets assign` will refuse it until it is (#10368)"
    )
}
