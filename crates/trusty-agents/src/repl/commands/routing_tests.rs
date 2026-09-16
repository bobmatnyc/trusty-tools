//! The `/local` toggle's config-load failure arms (#7609 slice 7).
//!
//! Why: this is the fail-open check for the slice. `persist_local_inference`
//! replaced `load_or_create().await.unwrap_or_default()` followed by a `save()`
//! — a shape that converted ANY load failure into the documented defaults and
//! then published them. Both tests here fail against that shape: the first
//! because a default config appears where the load failed, the second because
//! the operator's file is replaced by defaults.
//! Test: this module IS the test.

use super::persist_local_inference;

/// `$HOME` pointed at a fresh tempdir, returning it and the config path.
fn sandbox_home() -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().expect("tempdir");
    let path = home.path().join(".trusty-agents").join("config.toml");
    unsafe {
        std::env::set_var("HOME", home.path());
    }
    (home, path)
}

/// A config that cannot be created is REPORTED, and no default file is written.
///
/// Why this shape: `~/.trusty-agents` is seeded as a FILE, so
/// `GlobalConfig::load_or_create` fails at `create_dir_all` — the earliest
/// failure on the path, before anything could have been written. The assertion
/// that matters is the ABSENCE of `config.toml` afterwards.
#[tokio::test]
async fn a_failed_config_load_writes_no_default_config() {
    let _home_guard = crate::test_env::lock_home();
    let (home, config_path) = sandbox_home();
    // `.trusty-agents` as a file makes the directory creation fail.
    std::fs::write(home.path().join(".trusty-agents"), "not a directory").expect("seed blocker");

    let mut out = String::new();
    let outcome = persist_local_inference(true, &mut out).await;

    assert!(outcome.is_none(), "the toggle reports failure: {out}");
    assert!(
        out.contains("failed to read config"),
        "the reason reaches the operator: {out}"
    );
    assert!(
        out.contains("nothing was written"),
        "and says nothing changed: {out}"
    );
    assert!(
        !config_path.exists(),
        "no default config was materialised at {}",
        config_path.display()
    );
}

/// A config that will not parse is never replaced by the defaults.
///
/// Why: the damaging arm. `unwrap_or_default()` produced a full default
/// `GlobalConfig` from an unparseable file, and the `save()` immediately after
/// it wrote that default over the operator's real `[mcp]`, `[github]` and
/// `[[channels]]`.
#[tokio::test]
async fn a_config_that_will_not_parse_is_never_replaced_by_defaults() {
    let _home_guard = crate::test_env::lock_home();
    let (_home, config_path) = sandbox_home();
    std::fs::create_dir_all(config_path.parent().expect("config dir")).expect("config dir");
    let broken = "[mcp]\ninject_for_roles = [\"ctrl\"\n";
    std::fs::write(&config_path, broken).expect("seed broken config");

    let mut out = String::new();
    let outcome = persist_local_inference(true, &mut out).await;

    assert!(outcome.is_none(), "the toggle reports failure: {out}");
    assert_eq!(
        std::fs::read_to_string(&config_path).expect("config still there"),
        broken,
        "the operator's file is byte-for-byte what it was"
    );
}
