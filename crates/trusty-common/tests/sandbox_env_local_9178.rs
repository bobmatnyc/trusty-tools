//! `TRUSTY_SANDBOX=1` stops every `.env.local` read, end to end (#9178).
//!
//! Why: the unit tests in `credentials::dotenv` drive the hermetic core with
//! the flag as a parameter. This binary proves the production wrappers read
//! the flag from the process environment. It sets process-wide state —
//! `TRUSTY_SANDBOX`, `HOME`, the cwd — and the loader's once-only latch, so it
//! is one test in its own test binary.
//! Test: `sandbox_env_local_9178_reads_no_tier`.
#![cfg(feature = "credentials")]

use trusty_common::credentials::{
    env_local_value, find_workspace_env_local, load_env_local_once, read_var_from_env_local,
    user_env_local_path,
};

const PROJECT_VAR: &str = "CANARY_9178_PROJECT";
const HOME_VAR: &str = "CANARY_9178_HOME";

/// Why: a sandboxed daemon reached the developer's credentials through
/// `.env.local` even under `env -i` (#9178).
/// What: builds a project tree and a home directory, each with a canary
/// `.env.local`, points the cwd and `HOME` at them, sets `TRUSTY_SANDBOX=1`,
/// and asserts the loader and both inspectors see neither canary. It then
/// clears the flag and calls the loader again: the latch already fired, so
/// nothing loads.
/// Test: itself.
#[test]
fn sandbox_env_local_9178_reads_no_tier() {
    let project = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join(".git")).unwrap();
    let project_env = project.path().join(".env.local");
    std::fs::write(&project_env, format!("{PROJECT_VAR}=x\n")).unwrap();
    let cwd = project.path().join("crates").join("daemon");
    std::fs::create_dir_all(&cwd).unwrap();

    let home = tempfile::TempDir::new().unwrap();
    std::fs::write(home.path().join(".env.local"), format!("{HOME_VAR}=x\n")).unwrap();

    // Both tiers are reachable, so only the flag can stop them.
    assert_eq!(find_workspace_env_local(&cwd), Some(project_env.clone()));
    assert!(user_env_local_path(home.path()).is_some());

    // SAFETY: the only test in this binary; no other thread reads the env.
    unsafe {
        std::env::remove_var(PROJECT_VAR);
        std::env::remove_var(HOME_VAR);
        std::env::set_var("HOME", home.path());
        std::env::set_var("TRUSTY_SANDBOX", "1");
    }
    std::env::set_current_dir(&cwd).unwrap();

    load_env_local_once();
    assert_eq!(std::env::var_os(PROJECT_VAR), None, "project tier loaded");
    assert_eq!(std::env::var_os(HOME_VAR), None, "home tier loaded");
    assert_eq!(env_local_value(PROJECT_VAR), None);
    assert_eq!(read_var_from_env_local(&project_env, PROJECT_VAR), None);

    // SAFETY: as above.
    unsafe {
        std::env::remove_var("TRUSTY_SANDBOX");
    }
    load_env_local_once();
    assert_eq!(std::env::var_os(PROJECT_VAR), None, "the latch re-fired");
    assert_eq!(std::env::var_os(HOME_VAR), None, "the latch re-fired");
}
