//! Tests for [`super`] — the `rust_build_env` repair (#6868).
//!
//! Why: every write here targets a path under the operator's `$HOME` in
//! production, so every test points `config_path` and `cargo_target_dir` at a
//! `tempfile::TempDir` instead. Nothing in this file can reach the real
//! `~/.trusty-tools/trusty-mpm/config.yaml` or `~/.cargo/config.toml`.
//! Test: this file.

use std::path::Path;

use tempfile::TempDir;

use super::*;
use crate::core::build_env::BuildConfig;

/// A resolved environment rooted in `home`.
fn env_at(home: &Path) -> ResolvedBuildEnv {
    ResolvedBuildEnv {
        cargo_target_dir: home
            .join(".trusty-tools")
            .join("cargo-target")
            .join("bobmatnyc")
            .join("trusty-tools"),
        build_jobs: 8,
        sccache: false,
        target_dir_configured: false,
    }
}

/// `<home>/.trusty-tools/trusty-mpm/config.yaml`.
fn config_path_at(home: &Path) -> std::path::PathBuf {
    home.join(".trusty-tools")
        .join("trusty-mpm")
        .join("config.yaml")
}

/// Every step names the check that reported it.
///
/// Why: `--fix` prints `[<check>] <path>` and an operator maps the line back to
/// the row by that literal.
#[test]
fn rust_build_env_repair_steps_name_the_check() {
    let home = TempDir::new().expect("temp home");
    let steps = repair_rust_build_env(
        &config_path_at(home.path()),
        &env_at(home.path()),
        RepairMode::DryRun,
    );
    assert!(!steps.is_empty(), "a bare temp home must have work to do");
    for step in &steps {
        assert_eq!(step.check, "rust_build_env");
    }
}

/// Applying once creates the directory and writes the defaults, and the file
/// reads back as the resolved values.
#[test]
fn rust_build_env_repair_creates_dir_and_writes_defaults_once() {
    let home = TempDir::new().expect("temp home");
    let env = env_at(home.path());
    let config = config_path_at(home.path());
    std::fs::create_dir_all(config.parent().expect("parent")).expect("config dir");
    std::fs::write(&config, "auto_resume: true\n").expect("seed an existing config");

    let steps = repair_rust_build_env(&config, &env, RepairMode::Apply);

    assert_eq!(
        steps.len(),
        2,
        "one directory step, one config step: {steps:?}"
    );
    for step in &steps {
        assert!(
            matches!(step.status, StepStatus::Applied { .. }),
            "every step must apply: {step:?}"
        );
    }
    assert!(env.cargo_target_dir.is_dir(), "the target directory exists");

    // The pre-existing key survived, and the seeded one reads back.
    let raw = std::fs::read_to_string(&config).expect("read back");
    assert!(raw.contains("auto_resume: true"), "{raw}");
    #[derive(serde::Deserialize)]
    struct Doc {
        auto_resume: Option<bool>,
        build: BuildConfig,
    }
    let doc: Doc = serde_yaml::from_str(&raw).expect("the appended file must still parse");
    assert_eq!(doc.auto_resume, Some(true));
    assert_eq!(doc.build.build_jobs, Some(8));
    assert_eq!(
        doc.build.cargo_target_dir.as_deref(),
        Some(env.cargo_target_dir.display().to_string().as_str())
    );
    assert_eq!(doc.build.sccache, Some(false));

    // The existing file was backed up byte-identically before the append.
    let backups: Vec<_> = std::fs::read_dir(config.parent().expect("parent"))
        .expect("list")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".bak-"))
        .collect();
    assert_eq!(backups.len(), 1, "exactly one backup: {backups:?}");
    assert_eq!(
        std::fs::read_to_string(backups[0].path()).expect("read backup"),
        "auto_resume: true\n"
    );
}

/// A second apply changes nothing and reports nothing.
///
/// Why: this is what lets an operator run `--fix` on a schedule. A repair that
/// re-appends its own block would produce a duplicate-key YAML document on run
/// two.
#[test]
fn rust_build_env_repair_is_idempotent() {
    let home = TempDir::new().expect("temp home");
    let env = env_at(home.path());
    let config = config_path_at(home.path());

    let first = repair_rust_build_env(&config, &env, RepairMode::Apply);
    assert_eq!(first.len(), 2, "the first run does both halves: {first:?}");
    let after_first = std::fs::read_to_string(&config).expect("written");

    let second = repair_rust_build_env(&config, &env, RepairMode::Apply);
    assert!(
        second.is_empty(),
        "a second run must find nothing to repair: {second:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&config).expect("unchanged"),
        after_first,
        "the second run must not touch the file"
    );

    // And a third run through the dry-run path is equally silent.
    assert!(repair_rust_build_env(&config, &env, RepairMode::DryRun).is_empty());
}

/// An existing `build:` section is never rewritten, however partial.
///
/// Why: the operator's values are the answer, including the ones they left
/// unset. Seeding "the rest" would install defaults they deliberately omitted
/// and silently change what every build on the host does.
#[test]
fn rust_build_env_repair_never_overwrites_existing_build_keys() {
    let home = TempDir::new().expect("temp home");
    let env = env_at(home.path());
    let config = config_path_at(home.path());
    std::fs::create_dir_all(config.parent().expect("parent")).expect("config dir");
    // Only ONE of the three keys, and a value that differs from every default.
    let original = "build:\n  build_jobs: 1\n";
    std::fs::write(&config, original).expect("seed");

    let steps = repair_rust_build_env(&config, &env, RepairMode::Apply);

    assert!(
        !steps.iter().any(|s| s.path == config),
        "no step may target the config file: {steps:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&config).expect("read back"),
        original,
        "the file must be byte-identical"
    );

    // The directory half still ran — the two halves are independent.
    assert!(env.cargo_target_dir.is_dir());
}

/// A config file that does not parse is refused, never appended to.
///
/// Why: fail closed. Appending a valid block to a broken document produces a
/// file that is still broken, with tm's fingerprints on it.
#[test]
fn rust_build_env_repair_refuses_an_unparseable_config() {
    let home = TempDir::new().expect("temp home");
    let env = env_at(home.path());
    let config = config_path_at(home.path());
    std::fs::create_dir_all(config.parent().expect("parent")).expect("config dir");
    let broken = "auto_resume: true\n  bad: [indent\n";
    std::fs::write(&config, broken).expect("seed");

    let steps = repair_rust_build_env(&config, &env, RepairMode::Apply);
    let step = steps
        .iter()
        .find(|s| s.path == config)
        .expect("a config step");
    assert!(
        matches!(step.status, StepStatus::Refused(_)),
        "must refuse: {step:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&config).expect("read back"),
        broken,
        "a refused file must not be touched"
    );
}

/// `~/.cargo/config.toml` is refused with its reason, never written.
#[test]
fn rust_build_env_repair_refuses_to_wire_the_cargo_config() {
    let home = TempDir::new().expect("temp home");
    let mut env = env_at(home.path());
    env.sccache = true;
    let config = config_path_at(home.path());

    let steps = repair_rust_build_env(&config, &env, RepairMode::Apply);

    let cargo_config = home.path().join(".cargo").join("config.toml");
    let step = steps
        .iter()
        .find(|s| s.path == cargo_config)
        .expect("a step naming ~/.cargo/config.toml");
    match &step.status {
        StepStatus::Refused(why) => {
            assert_eq!(why, CARGO_CONFIG_REFUSAL);
            assert!(
                why.contains("machine-global"),
                "the reason must say why: {why}"
            );
        }
        other => panic!("must be refused, not {other:?}"),
    }
    assert!(
        !cargo_config.exists(),
        "the repair must never create ~/.cargo/config.toml"
    );

    // With sccache off there is nothing to refuse, so no such step at all.
    let mut off = env.clone();
    off.sccache = false;
    assert!(
        !repair_rust_build_env(&config, &off, RepairMode::DryRun)
            .iter()
            .any(|s| s.path == cargo_config)
    );
}

/// A directory that cannot be created is Failed, never silently skipped.
#[test]
fn rust_build_env_repair_reports_an_uncreatable_directory_as_failed() {
    let home = TempDir::new().expect("temp home");
    let mut env = env_at(home.path());
    // A regular FILE where the directory should be: `create_dir_all` cannot
    // win, and tm will not remove a file it did not write.
    let blocked = home.path().join("blocked");
    std::fs::write(&blocked, b"not a directory").expect("seed a file");
    env.cargo_target_dir = blocked.clone();

    let steps = repair_rust_build_env(&config_path_at(home.path()), &env, RepairMode::Apply);
    let step = steps
        .iter()
        .find(|s| s.path == blocked)
        .expect("a directory step");
    assert!(
        matches!(step.status, StepStatus::Refused(_)),
        "an occupied path must be refused, not overwritten: {step:?}"
    );
    assert_eq!(
        std::fs::read(&blocked).expect("still there"),
        b"not a directory",
        "the refused file must be untouched"
    );
}

/// An absent config file is created, carrying only the seeded block.
#[test]
fn rust_build_env_repair_seeds_an_absent_config_file() {
    let home = TempDir::new().expect("temp home");
    let env = env_at(home.path());
    let config = config_path_at(home.path());
    assert!(!config.exists(), "precondition: no config yet");

    let steps = repair_rust_build_env(&config, &env, RepairMode::Apply);
    assert!(steps.iter().any(|s| s.path == config && s.changed()));

    let raw = std::fs::read_to_string(&config).expect("created");
    assert!(raw.starts_with("build:"), "{raw}");
    assert!(
        !std::fs::read_dir(config.parent().expect("parent"))
            .expect("list")
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().contains(".bak-")),
        "there was nothing to back up"
    );
}

/// A dry run describes both halves and writes nothing.
#[test]
fn rust_build_env_repair_dry_run_writes_nothing() {
    let home = TempDir::new().expect("temp home");
    let env = env_at(home.path());
    let config = config_path_at(home.path());

    let steps = repair_rust_build_env(&config, &env, RepairMode::DryRun);
    assert_eq!(steps.len(), 2, "{steps:?}");
    assert!(steps.iter().all(|s| s.status == StepStatus::Planned));
    assert!(!config.exists(), "dry run must not create the config");
    assert!(
        !env.cargo_target_dir.exists(),
        "dry run must not create the directory"
    );
}
