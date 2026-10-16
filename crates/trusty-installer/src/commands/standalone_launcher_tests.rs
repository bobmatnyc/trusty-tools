//! Tests for `standalone_launcher.rs` (#9974).
//!
//! What: the `cargo install --list` parse, the `--force` decision, that a
//! failed install leaves the launcher in place, and that both tctl cargo
//! fallbacks route through the decision. The cargo runner is a recording
//! fake; only the ignore-tagged test spawns real cargo, offline, on path
//! fixtures.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use super::*;

/// `cargo install --list` after `cargo install trusty-embedderd-py` 0.1.x
/// and an older trusty-search.
fn list_with_standalone() -> String {
    format!(
        "trusty-embedderd-py v0.1.4:\n    {LAUNCHER_BIN}{exe}\n\
         trusty-search v0.60.0:\n    trusty-embedderd{exe}\n    trusty-search{exe}\n",
        exe = std::env::consts::EXE_SUFFIX
    )
}

/// The same machine after trusty-search owns all three binaries, plus a
/// package whose name only starts with the standalone one.
const LIST_WITHOUT_STANDALONE: &str = "\
trusty-embedderd-py-extras v0.1.0 (/src/extras):
    trusty-embedderd-py
trusty-search v0.61.0:
    trusty-embedderd
    trusty-embedderd-py
    trusty-search
";

/// Records each cargo call against a temp install root. `install` models a
/// failed guarded install: cargo fails and the #5777 guard puts every binary
/// back, so the root is left as it was (the guard's restore is proven in
/// trusty-common's `failed_guarded_install_keeps_the_launcher`).
struct FakeCargo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    list: Result<String, String>,
    install_ok: bool,
    calls: RefCell<Vec<String>>,
}

impl FakeCargo {
    fn new(list: Result<String, String>, install_ok: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("bin")).expect("bin dir");
        std::fs::write(root.join("bin").join(LAUNCHER_BIN), "standalone 0.1.4")
            .expect("seed launcher");
        Self {
            _dir: dir,
            root,
            list,
            install_ok,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn launcher(&self) -> PathBuf {
        self.root.join("bin").join(LAUNCHER_BIN)
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl CargoRunner for FakeCargo {
    async fn list_installed(&self) -> anyhow::Result<String> {
        self.calls.borrow_mut().push("install --list".to_owned());
        self.list.clone().map_err(anyhow::Error::msg)
    }

    async fn install(&self, crate_name: &str, force: bool) -> anyhow::Result<()> {
        let args = if force {
            format!("install {crate_name} --locked --force")
        } else {
            format!("install {crate_name} --locked")
        };
        self.calls.borrow_mut().push(args);
        if self.install_ok {
            Ok(())
        } else {
            anyhow::bail!("error: failed to query the crates.io index (offline)")
        }
    }
}

fn run(crate_name: &str, runner: &FakeCargo) -> anyhow::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(install_crate(crate_name, runner))
}

#[test]
fn record_with_standalone_launcher_forces_the_install() {
    let fake = FakeCargo::new(Ok(list_with_standalone()), true);
    run("trusty-search", &fake).expect("install runs");
    assert_eq!(
        fake.calls(),
        ["install --list", "install trusty-search --locked --force"],
        "a recorded standalone launcher must be taken over with --force"
    );
}

#[test]
fn no_record_installs_without_force() {
    let fake = FakeCargo::new(Ok(LIST_WITHOUT_STANDALONE.to_owned()), true);
    run("trusty-search", &fake).expect("install runs");
    assert_eq!(
        fake.calls(),
        ["install --list", "install trusty-search --locked"],
        "no standalone record must install exactly as before, without --force"
    );
}

#[test]
fn failed_listing_installs_without_force() {
    let fake = FakeCargo::new(
        Err("error: failed to parse crate metadata".to_owned()),
        true,
    );
    run("trusty-search", &fake).expect("install runs");
    assert_eq!(
        fake.calls(),
        ["install --list", "install trusty-search --locked"]
    );
}

#[test]
fn crate_without_the_launcher_never_lists_or_forces() {
    for crate_name in ["trusty-memory", STANDALONE_PACKAGE] {
        let fake = FakeCargo::new(Ok(list_with_standalone()), true);
        run(crate_name, &fake).expect("install runs");
        assert_eq!(
            fake.calls(),
            [format!("install {crate_name} --locked")],
            "{crate_name}"
        );
    }
}

#[test]
fn failed_install_keeps_the_launcher() {
    let fake = FakeCargo::new(Ok(list_with_standalone()), false);
    let err = run("trusty-search", &fake).expect_err("the install fails");
    assert!(err.to_string().contains("offline"), "{err}");
    assert!(
        fake.launcher().exists(),
        "a failed install must leave {} in place; calls: {:?}",
        fake.launcher().display(),
        fake.calls()
    );
    assert!(
        fake.calls().iter().all(|c| c.starts_with("install ")),
        "only `cargo install` may run: {:?}",
        fake.calls()
    );
}

/// Why (#9974): a call site that calls `perform_upgrade*` directly skips the
/// takeover decision, and cargo refuses the install on a machine with the
/// standalone launcher. Neither cargo fallback has a test seam of its own.
/// What: every non-comment `perform_upgrade` call in this crate's `src/` must
/// sit in `standalone_launcher.rs`, or in `self_update.rs` for
/// trusty-installer (which does not ship the launcher); and `install.rs` and
/// `upgrade.rs` must each call `standalone_launcher::cargo_install`.
/// Test: this is the test.
#[test]
fn cargo_fallbacks_route_through_the_takeover_decision() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.ends_with(".rs") || name.starts_with("standalone_launcher") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("source readable");
            for (n, line) in text.lines().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") || !t.contains("perform_upgrade") {
                    continue;
                }
                if name == "self_update.rs" && t.contains("perform_upgrade(\"trusty-installer\")") {
                    continue;
                }
                offenders.push(format!("{}:{}: {t}", path.display(), n + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "cargo fallbacks must call standalone_launcher::cargo_install: {offenders:#?}"
    );
    for file in ["install.rs", "upgrade.rs"] {
        let text = std::fs::read_to_string(src.join("commands").join(file)).expect("read");
        assert!(
            text.contains("standalone_launcher::cargo_install("),
            "{file} must route its cargo fallback through the takeover decision"
        );
    }
}

/// Write a path-crate fixture with the given package name and binaries.
fn write_fixture(dir: &Path, package: &str, version: &str, bins: &[&str]) {
    std::fs::create_dir_all(dir.join("src")).expect("fixture dir");
    let mut manifest = format!(
        "[package]\nname = \"{package}\"\nversion = \"{version}\"\nedition = \"2021\"\n\n[workspace]\n"
    );
    for bin in bins {
        let file = format!("src/{}.rs", bin.replace('-', "_"));
        std::fs::write(dir.join(&file), "fn main() {}\n").expect("bin source");
        manifest.push_str(&format!("\n[[bin]]\nname = \"{bin}\"\npath = \"{file}\"\n"));
    }
    std::fs::write(dir.join("Cargo.toml"), manifest).expect("manifest");
}

/// Run real cargo with an isolated target dir; returns (success, stdout+stderr).
fn real_cargo(args: &[&str], target: &Path) -> (bool, String) {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = std::process::Command::new(cargo)
        .args(args)
        .env("CARGO_TARGET_DIR", target)
        .output()
        .expect("spawn cargo");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

/// Why (#9974): proves on real cargo what the unit tests assume — a plain
/// install refuses over the standalone record, and `--force` installs and
/// drops the record, even when the recorded binary file is already missing.
/// What: offline path fixtures stand in for trusty-embedderd-py 0.1.x and
/// trusty-search; checks [`launcher_recorded_in_list`] on real
/// `cargo install --list` output before and after.
/// Test: this is the test.
#[test]
#[ignore = "runs real cargo install on offline path fixtures; run with --include-ignored"]
fn real_cargo_force_install_drops_the_standalone_record() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (old, new) = (tmp.path().join("old"), tmp.path().join("new"));
    let (root, target) = (tmp.path().join("root"), tmp.path().join("target"));
    write_fixture(&old, STANDALONE_PACKAGE, "0.1.4", &[LAUNCHER_BIN]);
    write_fixture(
        &new,
        "trusty-search",
        "0.61.0",
        &["trusty-search", LAUNCHER_BIN],
    );
    let (root_s, old_s, new_s) = (
        root.to_str().expect("utf8"),
        old.to_str().expect("utf8"),
        new.to_str().expect("utf8"),
    );
    let list = |target: &Path| real_cargo(&["install", "--list", "--root", root_s], target);

    let (ok, log) = real_cargo(
        &["install", "--offline", "--path", old_s, "--root", root_s],
        &target,
    );
    assert!(ok, "seed install failed: {log}");
    let (ok, listing) = list(&target);
    assert!(ok && launcher_recorded_in_list(&listing), "{listing}");

    let (ok, log) = real_cargo(
        &["install", "--offline", "--path", new_s, "--root", root_s],
        &target,
    );
    assert!(
        !ok,
        "a plain install must refuse over the standalone record: {log}"
    );

    // The MEDIUM trap: record present, binary file missing.
    let launcher = root
        .join("bin")
        .join(format!("{LAUNCHER_BIN}{}", std::env::consts::EXE_SUFFIX));
    std::fs::remove_file(&launcher).expect("remove launcher");
    let (ok, log) = real_cargo(
        &[
            "install",
            "--offline",
            "--force",
            "--path",
            new_s,
            "--root",
            root_s,
        ],
        &target,
    );
    assert!(ok, "--force must install over the standalone record: {log}");
    assert!(launcher.exists(), "trusty-search must own the launcher now");
    let (ok, listing) = list(&target);
    assert!(ok && !launcher_recorded_in_list(&listing), "{listing}");
    let crates2 = std::fs::read_to_string(root.join(".crates2.json")).expect("crates2");
    assert!(!crates2.contains("trusty-embedderd-py 0.1.4"), "{crates2}");
}
