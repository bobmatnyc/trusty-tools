//! Tests for where the 1Password backend finds `op` (#7519, #7524 P2-M2):
//! the machine pin, else a fixed list of system directories, never a `PATH`.
//!
//! Every directory list is a value handed to `open_in`; no test reads or
//! changes the process's `PATH`. A planted `op` creates a marker file if it
//! ever runs. The production `open` is called only to open, which spawns
//! nothing, so a real `op` in a system directory never runs.
//!
//! Test: itself.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::TempDir;

use super::program::INSTALL_HINT;
use super::shim::{OpShim, plant_op, relative_to_cwd};
use super::{OnePasswordBackend, OnePasswordSettings, open, open_in};
use crate::api::{SecretKey, SecretsError, VaultName};
use crate::store::SecretBackend;
use crate::store::program::{ONEPASSWORD_DIRS, find_on_path};

/// A machine config that enables 1Password with no settings.
const ENABLED: &str = "secrets:\n  onepassword: {}\n";

/// A temp dir holding a planted `op`, its marker, and an enabling machine
/// config.
struct Planted {
    tmp: TempDir,
    marker: PathBuf,
    /// The planted `op`'s directory, absolute.
    dir: PathBuf,
    machine: PathBuf,
}

impl Planted {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let marker = tmp.path().join("planted-ran");
        let dir = tmp.path().join("planted");
        plant_op(&dir, &marker);
        let machine = tmp.path().join("machine.yaml");
        std::fs::write(&machine, ENABLED).unwrap();
        Self {
            tmp,
            marker,
            dir,
            machine,
        }
    }

    fn templates(&self) -> PathBuf {
        self.tmp.path().join("tmp")
    }

    /// Entries that lead to the planted `op` without being absolute.
    fn relative_entries(&self) -> Vec<PathBuf> {
        let relative = relative_to_cwd(&self.dir);
        vec![
            PathBuf::new(),
            PathBuf::from("."),
            Path::new(".").join(&relative),
            relative,
        ]
    }

    fn open_in(&self, dirs: &[PathBuf]) -> Result<Arc<dyn SecretBackend>, SecretsError> {
        open_in(&self.machine, &self.templates(), None, dirs)
    }

    fn assert_never_ran(&self) {
        assert!(!self.marker.exists(), "the planted `op` ran");
    }
}

fn vault() -> VaultName {
    VaultName::new("trusty/acme/web").unwrap()
}

fn key() -> SecretKey {
    SecretKey::new("API_KEY").unwrap()
}

/// Why: #7524 P2-M2, Architect Decision A — a `PATH` that lists a planted
/// `op` ahead of the system directories made that `op` the program, so it
/// received item templates, values inside. The production open reads no
/// `PATH`: the planted `op` the `PATH` lookup would pick is never chosen.
/// Red while `open` searched a `PATH` handed in by the binary.
/// Test: itself.
#[test]
fn onepassword_op_on_the_spawner_path_is_never_chosen() {
    let planted = Planted::new();
    let mut entries = vec![planted.dir.clone()];
    entries.extend(ONEPASSWORD_DIRS.iter().map(PathBuf::from));
    let hostile = std::env::join_paths(&entries).unwrap();
    assert_eq!(
        find_on_path("op", Some(hostile.as_os_str())),
        Some(planted.dir.join("op")),
        "the spawner's PATH would have chosen the planted `op`"
    );

    match open(&planted.machine, &planted.templates(), None) {
        Ok(backend) => {
            let shown = format!("{backend:?}");
            assert!(
                !shown.contains(&planted.dir.display().to_string()),
                "{shown}"
            );
            assert!(
                ONEPASSWORD_DIRS.iter().any(|dir| shown.contains(dir)),
                "{shown}"
            );
        }
        Err(SecretsError::CliNotInstalled { program, .. }) => assert_eq!(program, "op"),
        Err(other) => panic!("expected an `op` from a system directory, got {other:?}"),
    }
    planted.assert_never_ran();
}

/// Why: #7524 P2-M2 — without a pin, `op` is the first executable one in
/// the system directories, in order. A relative or empty entry names the
/// working directory, so it is skipped even inside the list.
/// Test: itself.
#[test]
fn onepassword_resolves_op_from_the_system_dirs_in_order() {
    let planted = Planted::new();
    let shim = OpShim::new();
    let empty = planted.tmp.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let first = shim.install_in(&planted.tmp.path().join("first"));
    let second_marker = planted.tmp.path().join("second-ran");
    let second = plant_op(&planted.tmp.path().join("second"), &second_marker);
    let mut dirs = planted.relative_entries();
    dirs.extend([
        empty,
        first.parent().unwrap().to_path_buf(),
        second.parent().unwrap().to_path_buf(),
    ]);

    let backend = planted.open_in(&dirs).unwrap();
    let shown = format!("{backend:?}");
    assert!(shown.contains(&first.display().to_string()), "{shown}");
    assert!(backend.get(&vault(), &key()).unwrap().is_none());
    assert!(
        shim.calls()
            .starts_with("item list --vault trusty/acme/web"),
        "the first system `op` did not run: {}",
        shim.calls()
    );
    planted.assert_never_ran();
    assert!(!second_marker.exists(), "a later system `op` ran");
}

/// Why: #7524 P2-M2 — the machine `program` pin overrides everything, the
/// system directories included.
/// Test: itself.
#[test]
fn onepassword_machine_pin_overrides_the_system_dirs() {
    let planted = Planted::new();
    let shim = OpShim::new();
    let pinned = shim.install_in(&planted.tmp.path().join("pinned"));
    let yaml = format!(
        "secrets:\n  onepassword:\n    program: '{}'\n",
        pinned.display()
    );
    std::fs::write(&planted.machine, yaml).unwrap();

    let backend = planted.open_in(&[planted.dir.clone()]).unwrap();
    let shown = format!("{backend:?}");
    assert!(shown.contains(&pinned.display().to_string()), "{shown}");
    assert!(backend.get(&vault(), &key()).unwrap().is_none());
    assert!(!shim.calls().is_empty(), "the pinned `op` did not run");
    planted.assert_never_ran();
}

/// Why: #7524 P2-M2 — with no pin and no executable `op` in a system
/// directory, open is `CliNotInstalled`, and the message names the
/// `secrets.onepassword.program` pin. Nothing runs: not the planted `op`,
/// and not a relative program set on the settings directly.
/// Test: itself.
#[test]
fn onepassword_without_op_in_the_system_dirs_names_the_program_pin() {
    let planted = Planted::new();
    let not_executable = planted.tmp.path().join("noexec");
    std::fs::create_dir(&not_executable).unwrap();
    std::fs::write(not_executable.join("op"), "#!/bin/sh\n").unwrap();
    let directory = planted.tmp.path().join("dir");
    std::fs::create_dir_all(directory.join("op")).unwrap();

    for dirs in [
        Vec::new(),
        planted.relative_entries(),
        vec![not_executable.clone(), directory.clone()],
    ] {
        match planted.open_in(&dirs) {
            Err(err @ SecretsError::CliNotInstalled { .. }) => {
                let shown = err.to_string();
                assert!(shown.starts_with("`op` was not found"), "{shown}");
                assert!(shown.contains("secrets.onepassword.program"), "{shown}");
            }
            other => panic!("expected CliNotInstalled for {dirs:?}, got {other:?}"),
        }
    }

    let mut settings = OnePasswordSettings::new(planted.templates());
    settings.program = relative_to_cwd(&planted.dir).join("op").into_os_string();
    let err = OnePasswordBackend::new(settings)
        .get(&vault(), &key())
        .unwrap_err();
    assert!(
        matches!(err, SecretsError::CliNotInstalled { .. }),
        "{err:?}"
    );
    planted.assert_never_ran();
}

/// Why: #7524 P2-M2 — DOC-74 §6.2 states the directories searched; the
/// list and the hint that names it must not drift from it.
/// Test: itself.
#[test]
fn onepassword_system_dirs_are_the_documented_list() {
    #[cfg(target_os = "macos")]
    let expected = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].as_slice();
    #[cfg(not(target_os = "macos"))]
    let expected = ["/usr/local/bin", "/usr/bin"].as_slice();
    assert_eq!(ONEPASSWORD_DIRS, expected);
    for dir in ONEPASSWORD_DIRS {
        assert!(INSTALL_HINT.contains(dir), "{INSTALL_HINT}");
    }
    assert!(INSTALL_HINT.contains("secrets.onepassword.program"));
}
