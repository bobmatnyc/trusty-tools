//! Tests for where the 1Password backend finds `op` (#7519): the `PATH`
//! search at open, and the refusal to spawn a program that is not absolute.
//!
//! Every `PATH` is a value handed to `open`; no test reads or changes the
//! process's `PATH`. A planted `op` creates a marker file if it ever runs.
//! An empty entry and `.` name the working directory, the crate root, where
//! no test writes, so a planted `op` reaches the search only through a
//! relative entry that leads from there to a temp dir.
//!
//! Test: itself.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::program::find_on_path;
use super::shim::{OpShim, plant_op, relative_to_cwd};
use super::{OnePasswordBackend, OnePasswordSettings, open};
use crate::api::{SecretKey, SecretsError, VaultName};
use crate::store::SecretBackend;

/// A machine config that enables 1Password with no settings.
const ENABLED: &str = "secrets:\n  onepassword: {}\n";

/// A temp dir holding a planted `op`, its marker, and an enabling machine
/// config.
struct Planted {
    tmp: TempDir,
    marker: PathBuf,
    /// The planted `op`'s directory, relative to the working directory.
    relative: PathBuf,
    machine: PathBuf,
}

impl Planted {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let marker = tmp.path().join("planted-ran");
        let dir = tmp.path().join("planted");
        plant_op(&dir, &marker);
        let relative = relative_to_cwd(&dir);
        let machine = tmp.path().join("machine.yaml");
        std::fs::write(&machine, ENABLED).unwrap();
        Self {
            tmp,
            marker,
            relative,
            machine,
        }
    }

    fn templates(&self) -> PathBuf {
        self.tmp.path().join("tmp")
    }

    /// The `PATH` entries that lead to the planted `op` without being
    /// absolute: empty, `.`, the relative directory, and `./` before it.
    fn relative_entries(&self) -> Vec<PathBuf> {
        vec![
            PathBuf::new(),
            PathBuf::from("."),
            self.relative.clone(),
            Path::new(".").join(&self.relative),
        ]
    }

    fn open(
        &self,
        search: Option<&OsString>,
    ) -> Result<std::sync::Arc<dyn SecretBackend>, SecretsError> {
        open(
            &self.machine,
            &self.templates(),
            None,
            search.map(OsString::as_os_str),
        )
    }
}

fn joined(entries: &[PathBuf]) -> OsString {
    std::env::join_paths(entries).unwrap()
}

fn vault() -> VaultName {
    VaultName::new("trusty/acme/web").unwrap()
}

fn key() -> SecretKey {
    SecretKey::new("API_KEY").unwrap()
}

/// Why: #7519 — a planted `op` under a relative entry, an empty entry or
/// `.` would receive item templates, values inside. The search skips every
/// entry that is not absolute, so the later absolute `op` is the program,
/// and the planted one never runs. Red when the search accepts a relative
/// entry.
/// Test: itself.
#[test]
fn onepassword_path_search_skips_relative_empty_and_dot_entries() {
    let planted = Planted::new();
    let shim = OpShim::new();
    let real = shim.install_in(&planted.tmp.path().join("bin"));
    let mut entries = planted.relative_entries();
    entries.push(real.parent().unwrap().to_path_buf());
    let search = joined(&entries);

    assert_eq!(
        find_on_path("op", Some(search.as_os_str())),
        Some(real.clone())
    );
    let backend = planted.open(Some(&search)).unwrap();
    let shown = format!("{backend:?}");
    assert!(shown.contains(&real.display().to_string()), "{shown}");
    assert!(backend.get(&vault(), &key()).unwrap().is_none());
    assert!(
        shim.calls()
            .starts_with("item list --vault trusty/acme/web"),
        "the absolute `op` did not run: {}",
        shim.calls()
    );
    assert!(!planted.marker.exists(), "the planted `op` ran");
}

/// Why: #7519 — with no executable `op` in an absolute `PATH` entry, open
/// is `CliNotInstalled` with the install-or-pin hint, and nothing runs: not
/// the planted `op`, and not a relative program set on the settings
/// directly. Red when the search accepts a relative entry or a
/// non-executable file, or the backend spawns a program that is not
/// absolute.
/// Test: itself.
#[test]
fn onepassword_path_without_an_absolute_op_is_cli_not_installed() {
    let planted = Planted::new();
    let not_executable = planted.tmp.path().join("noexec");
    std::fs::create_dir(&not_executable).unwrap();
    std::fs::write(not_executable.join("op"), "#!/bin/sh\n").unwrap();
    let directory = planted.tmp.path().join("dir");
    std::fs::create_dir_all(directory.join("op")).unwrap();

    for search in [
        None,
        Some(OsString::new()),
        Some(joined(&planted.relative_entries())),
        Some(joined(&[not_executable.clone(), directory.clone()])),
    ] {
        assert_eq!(find_on_path("op", search.as_deref()), None, "{search:?}");
        match planted.open(search.as_ref()) {
            Err(SecretsError::CliNotInstalled { program, hint }) => {
                assert_eq!(program, "op");
                assert!(hint.contains("secrets.onepassword.program"), "{hint}");
            }
            other => panic!("expected CliNotInstalled for {search:?}, got {other:?}"),
        }
    }

    let mut settings = OnePasswordSettings::new(planted.templates());
    settings.program = planted.relative.join("op").into_os_string();
    let err = OnePasswordBackend::new(settings)
        .get(&vault(), &key())
        .unwrap_err();
    assert!(
        matches!(err, SecretsError::CliNotInstalled { .. }),
        "{err:?}"
    );
    assert!(!planted.marker.exists(), "the planted `op` ran");
}
