//! `tm doctor`'s gcloud auth row is opt-in behind `--network` (#8371).
//!
//! Why: owner ruling 4d — a bare `tm doctor` makes no network call and never
//! spawns `gcloud`. The unit tests prove the gate function; only the real
//! binary proves the CLI passes the flag through and that a token minted by a
//! real child process never reaches stdout or stderr.
//! What: puts a `gcloud` shim first on the child's `PATH`. The shim appends its
//! arguments to a marker file and answers `auth list` with one ACTIVE account
//! and `auth print-access-token` with a fake token. A bare run must leave the
//! marker absent; a `--network` run must call the shim and print neither the
//! token nor the account's local part.
//! Test: `cargo test -p trusty-mpm --test integration tm_doctor_network::`.

use std::path::{Path, PathBuf};

use crate::common;

/// The token the shim prints. No line of `tm doctor` output may contain it.
const SHIM_TOKEN: &str = "ya29.SHIM-SECRET-TOKEN-0123456789abcdef";

/// Write an executable `gcloud` shim into `dir` that logs to `marker`.
fn write_shim(dir: &Path, marker: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        "#!/bin/sh\n\
         echo \"$@\" >> '{marker}'\n\
         case \"$2\" in\n\
         list) printf '%s' '[{{\"account\":\"shimuser@example.com\",\"status\":\"ACTIVE\"}}]' ;;\n\
         print-access-token) printf '%s\\n' '{SHIM_TOKEN}' ;;\n\
         *) exit 3 ;;\n\
         esac\n",
        marker = marker.display()
    );
    let path = dir.join("gcloud");
    std::fs::write(&path, script).expect("write gcloud shim");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod shim");
}

/// Run `tm doctor <extra>` with the shim first on `PATH`; return stdout,
/// stderr and the marker path.
fn run_doctor_with_shim(extra: &[&str]) -> (String, String, PathBuf, tempfile::TempDir) {
    let home = tempfile::tempdir().expect("scratch home");
    let cwd = tempfile::tempdir().expect("scratch cwd");
    let shim_dir = home.path().join("shim-bin");
    std::fs::create_dir_all(&shim_dir).expect("shim dir");
    let marker = home.path().join("gcloud-calls.log");
    write_shim(&shim_dir, &marker);
    let path = format!(
        "{}:{}",
        shim_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = common::tm_command_in(home.path())
        .env("PATH", path)
        .arg("doctor")
        .args(extra)
        .current_dir(cwd.path())
        .output()
        .expect("failed to spawn `tm doctor`");
    assert!(
        output.status.success(),
        "`tm doctor` exited non-zero.\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        marker,
        home,
    )
}

fn gcloud_row(stdout: &str) -> &str {
    stdout
        .lines()
        .find(|l| l.contains("gcloud_auth"))
        .unwrap_or_else(|| panic!("no gcloud_auth row.\nstdout:\n{stdout}"))
}

#[test]
fn tm_doctor_never_spawns_gcloud_without_network() {
    let (stdout, _stderr, marker, _home) = run_doctor_with_shim(&[]);
    assert!(
        !marker.exists(),
        "bare `tm doctor` spawned gcloud: {:?}",
        std::fs::read_to_string(&marker)
    );
    assert!(
        gcloud_row(&stdout).contains("skipped (needs --network)"),
        "{stdout}"
    );
}

#[test]
fn tm_doctor_network_reports_gcloud_auth_without_printing_the_token() {
    let (stdout, stderr, marker, _home) = run_doctor_with_shim(&["--network"]);
    let calls = std::fs::read_to_string(&marker).expect("the shim was called");
    assert!(calls.contains("auth list --format=json"), "{calls}");
    assert!(calls.contains("auth print-access-token"), "{calls}");
    let row = gcloud_row(&stdout);
    assert!(row.contains("s***@example.com can mint"), "{row}");
    for out in [&stdout, &stderr] {
        assert!(!out.contains(SHIM_TOKEN), "token printed:\n{out}");
        assert!(!out.contains("ya29."), "token prefix printed:\n{out}");
        assert!(
            !out.contains("shimuser@"),
            "account local part printed:\n{out}"
        );
    }
}
