//! First-run clone detection for the `tm` guided on-ramp (#1780).
//!
//! Why: the first `tm` invocation in a GitHub-backed project triggers a
//! synchronous `git clone` inside the daemon's HTTP handler — for a large
//! repo this can take minutes with zero user feedback. Detecting the absence
//! of the base clone BEFORE issuing the POST lets the client print a clear
//! "cloning…" message so the operator knows to wait.
//! What: exposes [`needs_first_run_clone`] as a single pure-I/O helper.
//! Test: `needs_first_run_clone_*` in `tests_behavior_c_tests.rs`.

/// Detect a first-run scenario where a base clone does not yet exist (#1780).
///
/// Why: see module-level doc.
/// What: returns `Some((owner/repo, base_clone_path))` when `repo_url` is a
/// local directory with a parseable GitHub origin remote AND the base clone
/// directory does not yet exist; returns `None` in every other case (not a dir,
/// no git remote, clone already present). Pure I/O (no network); always safe
/// to call before issuing the daemon POST.
/// Test: `needs_first_run_clone_*` in `tests_behavior_c_tests.rs`.
pub(crate) fn needs_first_run_clone(repo_url: &str) -> Option<(String, std::path::PathBuf)> {
    use trusty_mpm::daemon::managed_routes::inproject as ip;
    let p = std::path::Path::new(repo_url);
    if !p.is_dir() {
        return None;
    }
    let gh = trusty_common::github_path::parse_github_path(&ip::get_origin_url(p)?)?;
    let b = ip::base_clone_path(&gh.owner, &gh.repo);
    (!b.join(".git").exists()).then_some((format!("{}/{}", gh.owner, gh.repo), b))
}
