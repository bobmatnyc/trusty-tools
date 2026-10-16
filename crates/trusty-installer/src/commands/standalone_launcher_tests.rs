//! Tests for `standalone_launcher.rs` (#9974).
//!
//! What: the `.crates2.json` read, the uninstall-before-install plan, the
//! install-root rule, and the stop-on-failed-uninstall executor. The cargo
//! runner is a recording fake; no test spawns cargo.

use std::cell::RefCell;

use super::*;

/// A v2 metadata file as cargo writes it after
/// `cargo install trusty-embedderd-py` 0.1.x.
const WITH_STANDALONE: &str = r#"{
  "installs": {
    "trusty-embedderd-py 0.1.4 (registry+https://github.com/rust-lang/crates.io-index)": {
      "version_req": null,
      "bins": ["trusty-embedderd-py"],
      "features": [],
      "all_features": false,
      "no_default_features": false,
      "profile": "release",
      "target": "x86_64-unknown-linux-gnu",
      "rustc": "rustc 1.94.1"
    },
    "trusty-search 0.60.0 (registry+https://github.com/rust-lang/crates.io-index)": {
      "version_req": null,
      "bins": ["trusty-embedderd", "trusty-search"],
      "features": []
    }
  }
}"#;

/// The same machine after trusty-search owns all three binaries.
const WITHOUT_STANDALONE: &str = r#"{
  "installs": {
    "trusty-search 0.61.0 (registry+https://github.com/rust-lang/crates.io-index)": {
      "bins": ["trusty-embedderd", "trusty-embedderd-py", "trusty-search"]
    },
    "trusty-embedderd-py-extras 0.1.0 (registry+https://github.com/rust-lang/crates.io-index)": {
      "bins": ["trusty-embedderd-py-extras"]
    }
  }
}"#;

fn root_with(crates2: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    if let Some(text) = crates2 {
        std::fs::write(dir.path().join(".crates2.json"), text).expect("write fixture");
    }
    dir
}

/// Records each command; `fail_uninstall` makes the uninstall fail.
#[derive(Default)]
struct FakeCargo {
    calls: RefCell<Vec<String>>,
    fail_uninstall: bool,
}

impl CargoRunner for FakeCargo {
    async fn uninstall(&self, package: &str, root: &Path) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("uninstall {package} --root {}", root.display()));
        if self.fail_uninstall {
            return Err("error: package ID specification did not match".to_owned());
        }
        Ok(())
    }

    async fn install(&self, crate_name: &str) -> anyhow::Result<()> {
        self.calls
            .borrow_mut()
            .push(format!("install {crate_name}"));
        Ok(())
    }
}

fn run(steps: &[CargoStep], runner: &FakeCargo) -> anyhow::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(run_cargo_steps(steps, runner))
}

#[test]
fn fixture_with_standalone_launcher_plans_uninstall_before_install() {
    let root = root_with(Some(WITH_STANDALONE));
    let record = read_launcher_record(root.path());
    let steps = plan_cargo_install("trusty-search", Some(root.path()), record);
    let fake = FakeCargo::default();
    run(&steps, &fake).expect("plan runs");
    assert_eq!(
        *fake.calls.borrow(),
        vec![
            format!(
                "uninstall trusty-embedderd-py --root {}",
                root.path().display()
            ),
            "install trusty-search".to_owned(),
        ],
        "the standalone launcher must be uninstalled before trusty-search installs"
    );
}

#[test]
fn fixture_without_standalone_launcher_plans_no_uninstall() {
    for crates2 in [Some(WITHOUT_STANDALONE), None] {
        let root = root_with(crates2);
        let record = read_launcher_record(root.path());
        assert_eq!(record, LauncherRecord::NotRecorded, "{crates2:?}");
        let steps = plan_cargo_install("trusty-search", Some(root.path()), record);
        assert_eq!(
            steps,
            vec![CargoStep::Install {
                crate_name: "trusty-search".to_owned()
            }],
            "no standalone record must plan no uninstall ({crates2:?})"
        );
    }
}

#[test]
fn crate_without_the_launcher_plans_no_uninstall() {
    let root = root_with(Some(WITH_STANDALONE));
    for crate_name in ["trusty-memory", STANDALONE_PACKAGE] {
        let steps = plan_cargo_install(crate_name, Some(root.path()), LauncherRecord::Recorded);
        assert_eq!(steps.len(), 1, "{crate_name}: {steps:?}");
    }
}

#[test]
fn uninstall_failure_is_typed_and_skips_the_install() {
    let root = root_with(Some(WITH_STANDALONE));
    let record = read_launcher_record(root.path());
    let steps = plan_cargo_install("trusty-search", Some(root.path()), record);
    let fake = FakeCargo {
        fail_uninstall: true,
        ..FakeCargo::default()
    };
    let err = run(&steps, &fake).expect_err("a failed uninstall must stop the plan");
    let typed = err
        .downcast_ref::<StandaloneLauncherError>()
        .expect("typed StandaloneLauncherError");
    let StandaloneLauncherError::UninstallFailed { package, .. } = typed;
    assert_eq!(package, STANDALONE_PACKAGE);
    let msg = err.to_string();
    assert!(
        msg.contains("cargo uninstall trusty-embedderd-py`, then rerun"),
        "the error must name the manual fix: {msg}"
    );
    assert!(
        !fake.calls.borrow().iter().any(|c| c.starts_with("install")),
        "the install must not run after a failed uninstall: {:?}",
        fake.calls.borrow()
    );
}

#[test]
fn missing_crates2_is_not_recorded() {
    let root = root_with(None);
    assert_eq!(
        read_launcher_record(root.path()),
        LauncherRecord::NotRecorded
    );
}

#[test]
fn malformed_crates2_is_not_recorded() {
    for text in ["{ not json", r#"{"installs": []}"#, ""] {
        let root = root_with(Some(text));
        assert_eq!(
            read_launcher_record(root.path()),
            LauncherRecord::NotRecorded,
            "{text:?}"
        );
    }
}

#[test]
fn install_root_prefers_cargo_install_root() {
    let home = Path::new("/home/u");
    assert_eq!(
        cargo_install_root_from(Some(home), Some("/opt/cargo-root"), Some("/opt/cargo-home")),
        Some(PathBuf::from("/opt/cargo-root"))
    );
}

#[test]
fn install_root_falls_back_to_cargo_home_then_home() {
    let home = Path::new("/home/u");
    assert_eq!(
        cargo_install_root_from(Some(home), Some(""), Some("/opt/cargo-home")),
        Some(PathBuf::from("/opt/cargo-home"))
    );
    assert_eq!(
        cargo_install_root_from(Some(home), None, None),
        Some(PathBuf::from("/home/u/.cargo"))
    );
    assert_eq!(cargo_install_root_from(None, None, None), None);
}
