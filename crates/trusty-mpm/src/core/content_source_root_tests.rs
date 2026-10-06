//! #9298: `test_support::repo_root()` names the checkout the test RUNS for,
//! not the one that built the binary.
//!
//! Why: under a `CARGO_TARGET_DIR` shared across worktrees, a test binary
//! carried a compile-time path to its build worktree and read that tree's
//! content — or a reclaimed, deleted one. Only a second process can show this:
//! the driver re-execs this same binary with a different cwd and environment
//! and reads the root the child resolved.
//! What: [`repo_root_probe`] prints the resolved root when
//! [`PROBE_ENV`] is `1` and is a no-op otherwise. The driver builds a fake
//! checkout B, runs the probe from `B/crates/trusty-mpm` three ways, and
//! expects B, B, then the explicit root B2.
//! Test: `repo_root_follows_the_runtime_checkout_9298`.

use std::path::{Path, PathBuf};
use std::process::Command;

use trusty_common::test_harness::REPO_ROOT_ENV;

use super::test_support::repo_root;

/// Arms [`repo_root_probe`] in the child process.
const PROBE_ENV: &str = "TRUSTY_TEST_REPO_ROOT_PROBE";

/// The line prefix the probe prints and the driver parses.
const PREFIX: &str = "REPO_ROOT=";

/// Child half: prints the resolved root, only when the driver armed it.
#[test]
fn repo_root_probe() {
    if std::env::var_os(PROBE_ENV).is_none_or(|v| v != "1") {
        return;
    }
    let root = repo_root().canonicalize().expect("canonical repo root");
    println!("{PREFIX}{}", root.display());
}

/// A fake checkout at `dir`: a `[workspace]` manifest plus `crates/trusty-mpm/`.
fn fake_checkout(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir.join("crates/trusty-mpm")).expect("mkdir checkout");
    std::fs::write(
        dir.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    )
    .expect("write workspace manifest");
    dir.canonicalize().expect("canonical checkout")
}

/// One child run's outcome: the root it printed, or why it printed none.
fn probe(configure: impl FnOnce(&mut Command)) -> Result<PathBuf, String> {
    // The harness names a test by its path without the crate prefix.
    let module = module_path!()
        .split_once("::")
        .map_or(module_path!(), |(_, rest)| rest);
    let name = format!("{module}::repo_root_probe");
    let exe = std::env::current_exe().expect("current test binary");
    let mut cmd = Command::new(exe);
    cmd.args(["--exact", &name, "--nocapture", "--test-threads=1"])
        .env(PROBE_ENV, "1");
    configure(&mut cmd);
    let out = cmd.output().expect("re-exec the test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        // `--nocapture` prints on the harness's `test <name> ... ` line.
        .find_map(|line| line.split_once(PREFIX).map(|(_, root)| root.trim()))
        .map(PathBuf::from)
        .ok_or_else(|| {
            format!(
                "probe printed no {PREFIX} line ({}):\nstdout:\n{stdout}\nstderr:\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            )
        })
}

/// Driver half: the root follows the runtime checkout in all three cases.
#[test]
fn repo_root_follows_the_runtime_checkout_9298() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let b = fake_checkout(&tmp.path().join("b"));
    let b2 = fake_checkout(&tmp.path().join("b2"));
    let member = b.join("crates/trusty-mpm");

    // (name, runtime CARGO_MANIFEST_DIR, TRUSTY_TEST_REPO_ROOT, expected root);
    // the cwd is always B/crates/trusty-mpm. `None` removes the variable.
    let cases: [(&str, Option<&Path>, Option<&Path>, &Path); 3] = [
        (
            "(i) runtime CARGO_MANIFEST_DIR and cwd name B",
            Some(&member),
            None,
            &b,
        ),
        ("(ii) cwd names B, CARGO_MANIFEST_DIR unset", None, None, &b),
        (
            "(iii) TRUSTY_TEST_REPO_ROOT names B2",
            Some(&member),
            Some(&b2),
            &b2,
        ),
    ];
    for (name, manifest_dir, explicit, want) in cases {
        let got = probe(|cmd| {
            cmd.current_dir(&member);
            match manifest_dir {
                Some(dir) => cmd.env("CARGO_MANIFEST_DIR", dir),
                None => cmd.env_remove("CARGO_MANIFEST_DIR"),
            };
            match explicit {
                Some(dir) => cmd.env(REPO_ROOT_ENV, dir),
                None => cmd.env_remove(REPO_ROOT_ENV),
            };
        })
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(got, want, "{name}: resolved {}", got.display());
    }
}
