//! Tests for [`super`] — the `rust_build_env` doctor row (#6868).
//!
//! Why: the fold is driven from constructed [`BuildEnvFacts`] so no branch
//! depends on the test host having sccache installed, a warm target directory,
//! or a particular core count; the gatherer is driven against a `tempfile`
//! project and a `tempfile` home, so nothing here reads the operator's real
//! `~/.trusty-tools/trusty-mpm/config.yaml` or `~/.cargo/config.toml`.
//! Test: this file.

use tempfile::TempDir;

use super::*;

/// A resolved environment pointing at `dir`.
fn env_for(dir: &std::path::Path, sccache: bool) -> ResolvedBuildEnv {
    ResolvedBuildEnv {
        cargo_target_dir: dir.to_path_buf(),
        build_jobs: 8,
        sccache,
        target_dir_configured: false,
    }
}

/// Facts for a probed machine whose target directory is in `state`.
fn probed(state: TargetDirState, sccache: bool, wrapper: WrapperState) -> BuildEnvFacts {
    BuildEnvFacts::Probed {
        env: env_for(std::path::Path::new("/shared/target"), sccache),
        target_dir: state,
        sccache_on_path: None,
        wrapper,
    }
}

/// A non-Rust project reports the row as not applicable, never as a warning.
///
/// Why: `tm doctor` runs in every project tm manages. A Python or TypeScript
/// checkout has no cargo target directory to be missing, and a `Warn` there is
/// noise that trains an operator to ignore the row.
#[test]
fn rust_build_env_skips_on_a_non_rust_project() {
    // The fold's own branch.
    let check = build_check(&BuildEnvFacts::NotApplicable(
        "this project's detected stack does not include Rust".to_string(),
    ));
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("not applicable"),
        "{}",
        check.message
    );

    // And end to end, against a project whose only marker is a package.json.
    let project = TempDir::new().expect("temp project");
    let home = TempDir::new().expect("temp home");
    std::fs::write(project.path().join("package.json"), "{}").expect("marker");

    let check = check_rust_build_env(Some(project.path()), home.path());
    assert_eq!(check.name, CHECK_NAME);
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        !check.message.contains("CARGO_TARGET_DIR"),
        "a non-Rust project must not be handed a cargo paste line: {}",
        check.message
    );

    // No project directory at all is the same skip, never a failure.
    let none = check_rust_build_env(None, home.path());
    assert_eq!(none.status, CheckStatus::Ok, "{}", none.message);
}

/// A target directory that does not exist yet is a Warn carrying the fix.
#[test]
fn rust_build_env_warns_when_target_dir_missing() {
    let check = build_check(&probed(
        TargetDirState::Missing,
        false,
        WrapperState::NotWired,
    ));

    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(
        check.message.contains("DOES NOT EXIST"),
        "{}",
        check.message
    );
    assert!(
        check.message.contains("tm doctor --fix --yes"),
        "a Warn must name the command that fixes it: {}",
        check.message
    );

    // End to end, against a real missing path.
    let home = TempDir::new().expect("temp home");
    let missing = home.path().join("never-created");
    match probe_target_dir(&missing) {
        TargetDirState::Missing => {}
        other => panic!("expected Missing, got {other:?}"),
    }
}

/// A writable directory is Ok and reports what it holds.
#[test]
fn rust_build_env_ok_when_target_dir_writable() {
    let dir = TempDir::new().expect("temp dir");
    std::fs::write(dir.path().join("artifact.bin"), vec![0u8; 4096]).expect("write");

    let state = probe_target_dir(dir.path());
    match &state {
        TargetDirState::Writable { bytes, .. } => {
            assert!(*bytes >= 4096, "the walk must see the file: {bytes}");
        }
        other => panic!("expected Writable, got {other:?}"),
    }
    assert!(
        !dir.path().join(".tm-doctor-write-probe").exists(),
        "the write probe must not outlive the check"
    );

    let check = build_check(&BuildEnvFacts::Probed {
        env: env_for(dir.path(), false),
        target_dir: state,
        sccache_on_path: None,
        wrapper: WrapperState::NotWired,
    });
    assert_eq!(check.name, CHECK_NAME);
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("exists and is writable"),
        "{}",
        check.message
    );
}

/// A path that exists and cannot hold artifacts is the only Fail.
#[test]
fn rust_build_env_fails_when_target_dir_not_writable() {
    let check = build_check(&probed(
        TargetDirState::NotWritable("the path is not a directory".to_string()),
        false,
        WrapperState::NotWired,
    ));
    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(check.message.contains("NOT writable"), "{}", check.message);

    // End to end: a regular file where the directory should be.
    let home = TempDir::new().expect("temp home");
    let blocked = home.path().join("blocked");
    std::fs::write(&blocked, b"x").expect("seed a file");
    match probe_target_dir(&blocked) {
        TargetDirState::NotWritable(why) => assert!(why.contains("not a directory"), "{why}"),
        other => panic!("expected NotWritable, got {other:?}"),
    }
}

/// `sccache: true` with no wrapper wired warns and names the manual fix.
///
/// Why: the paste line this row prints would carry `RUSTC_WRAPPER=sccache`, so
/// every brief handed out from this machine would set a wrapper that is not
/// configured. `--fix` cannot repair it, so the row has to.
#[test]
fn rust_build_env_warns_when_sccache_requested_but_not_wired() {
    let check = build_check(&probed(
        TargetDirState::Writable {
            bytes: 0,
            partial: false,
        },
        true,
        WrapperState::NotWired,
    ));

    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(
        check.message.contains("`build.rustc-wrapper` NOT wired"),
        "{}",
        check.message
    );
    assert!(
        check.message.contains("machine-global"),
        "the row must say why tm will not wire it: {}",
        check.message
    );
    assert!(
        check.message.contains("RUSTC_WRAPPER=sccache"),
        "the paste line still reflects the config: {}",
        check.message
    );

    // Wired, and the same request is Ok.
    let wired = build_check(&probed(
        TargetDirState::Writable {
            bytes: 0,
            partial: false,
        },
        true,
        WrapperState::Wired("sccache".to_string()),
    ));
    assert_eq!(wired.status, CheckStatus::Ok, "{}", wired.message);
}

/// sccache absent and unrequested is reported, never warned about.
#[test]
fn rust_build_env_reports_sccache_without_warning_when_not_requested() {
    let check = build_check(&probed(
        TargetDirState::Writable {
            bytes: 0,
            partial: false,
        },
        false,
        WrapperState::NotWired,
    ));
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("sccache NOT on PATH"),
        "{}",
        check.message
    );
}

/// An underivable repo identity is UNDETERMINED, never a guessed path.
#[test]
fn rust_build_env_is_unknown_when_the_identity_cannot_be_derived() {
    let check = build_check(&BuildEnvFacts::Undetermined(
        "no `build.cargo_target_dir` is configured and the project has no parseable `origin` \
         remote. Set `build.cargo_target_dir` in `~/.trusty-tools/trusty-mpm/config.yaml`"
            .to_string(),
    ));
    assert_eq!(check.status, CheckStatus::Unknown, "{}", check.message);
    assert!(
        check.message.contains("build.cargo_target_dir"),
        "{}",
        check.message
    );
}

/// A directory tm could not measure never reads healthy.
#[test]
fn rust_build_env_is_unknown_when_the_directory_cannot_be_measured() {
    let check = build_check(&probed(
        TargetDirState::Unknown("permission denied".to_string()),
        false,
        WrapperState::NotWired,
    ));
    assert_eq!(check.status, CheckStatus::Unknown, "{}", check.message);
    assert!(
        check.message.contains("could not be measured"),
        "{}",
        check.message
    );
}

/// The paste line is present in every reporting branch.
///
/// Why: it is the row's deliverable. A branch that drops it silently costs the
/// PM the reason the row exists.
#[test]
fn every_probed_branch_carries_the_paste_line() {
    for state in [
        TargetDirState::Missing,
        TargetDirState::Writable {
            bytes: 1,
            partial: true,
        },
        TargetDirState::NotWritable("x".to_string()),
        TargetDirState::Unknown("y".to_string()),
    ] {
        let check = build_check(&probed(state.clone(), false, WrapperState::NotWired));
        assert!(
            check
                .message
                .contains("CARGO_TARGET_DIR=/shared/target CARGO_BUILD_JOBS=8 SKIP_UI_BUILD=1"),
            "{state:?} dropped the paste line: {}",
            check.message
        );
    }
}

/// An absent `~/.cargo/config.toml` is definitely not wired, not unknown.
#[test]
fn wrapper_is_not_wired_when_the_file_is_absent() {
    let home = TempDir::new().expect("temp home");
    assert_eq!(
        read_rustc_wrapper(&home.path().join(".cargo").join("config.toml")),
        WrapperState::NotWired
    );
}

/// The configured command is read verbatim.
#[test]
fn wrapper_reads_the_configured_command() {
    let home = TempDir::new().expect("temp home");
    let path = home.path().join("config.toml");
    std::fs::write(&path, "[build]\nrustc-wrapper = \"sccache\"\njobs = 4\n").expect("write");
    assert_eq!(
        read_rustc_wrapper(&path),
        WrapperState::Wired("sccache".to_string())
    );

    std::fs::write(&path, "[build]\njobs = 4\n").expect("write");
    assert_eq!(read_rustc_wrapper(&path), WrapperState::NotWired);
}

/// A malformed cargo config is UNKNOWN — it may well wire a wrapper.
#[test]
fn wrapper_is_unknown_for_a_malformed_file() {
    let home = TempDir::new().expect("temp home");
    let path = home.path().join("config.toml");
    std::fs::write(&path, "[build\nrustc-wrapper =").expect("write");
    match read_rustc_wrapper(&path) {
        WrapperState::Unknown(_) => {}
        other => panic!("expected Unknown, got {other:?}"),
    }
}

/// The gatherer resolves a Rust project against the temp home's own config.
///
/// Why: proves the wiring end to end — marker detection, config load from the
/// supplied `home`, and the default target directory — without touching the
/// operator's real config.
#[test]
fn a_rust_project_resolves_against_the_supplied_home() {
    let project = TempDir::new().expect("temp project");
    let home = TempDir::new().expect("temp home");
    std::fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .expect("marker");

    let config_dir = home.path().join(".trusty-tools").join("trusty-mpm");
    std::fs::create_dir_all(&config_dir).expect("config dir");
    let target = home.path().join("shared-target");
    std::fs::create_dir_all(&target).expect("target dir");
    std::fs::write(
        config_dir.join("config.yaml"),
        format!(
            "build:\n  cargo_target_dir: '{}'\n  build_jobs: 3\n",
            target.display()
        ),
    )
    .expect("config");

    let check = check_rust_build_env(Some(project.path()), home.path());
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("CARGO_BUILD_JOBS=3 SKIP_UI_BUILD=1"),
        "the configured job count must reach the paste line: {}",
        check.message
    );
    assert!(
        check.message.contains("from `build.cargo_target_dir`"),
        "the row must say where the path came from: {}",
        check.message
    );
    assert!(
        check.message.contains(&target.display().to_string()),
        "the configured directory must be the one reported: {}",
        check.message
    );
}
