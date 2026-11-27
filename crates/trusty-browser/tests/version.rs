//! `--version` on both binaries (#11802).
//!
//! Why: operators and installers check the installed build with `--version`;
//! before #11802 both clap parsers refused it as an unexpected argument.
//! What: runs each binary with `--version` and pins stdout to
//! `<bin> <CARGO_PKG_VERSION>` and the exit code to 0. Both binaries run
//! before the assertion, so a failure names every binary that is wrong.

use std::process::{Command, Stdio};

#[test]
fn version_flag_prints_crate_version_for_both_binaries() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut wrong = Vec::new();
    for (name, exe) in [
        ("trusty-browser", env!("CARGO_BIN_EXE_trusty-browser")),
        (
            "trusty-browser-mcp",
            env!("CARGO_BIN_EXE_trusty-browser-mcp"),
        ),
    ] {
        let out = Command::new(exe)
            .arg("--version")
            .env("HOME", home.path())
            .stdin(Stdio::null())
            .output()
            .expect("run");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let want = format!("{name} {}\n", env!("CARGO_PKG_VERSION"));
        if out.status.code() != Some(0) || stdout != want {
            wrong.push(format!(
                "{name}: exit {:?}, stdout {stdout:?}, want 0 and {want:?}; stderr {:?}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
