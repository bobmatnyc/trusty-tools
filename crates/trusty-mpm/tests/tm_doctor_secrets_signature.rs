//! `tm doctor` flags trusty-secrets code-signature drift (#11580).
//!
//! Why: the unit tests grade scripted probe answers; only the real binary
//! proves `tm doctor` runs the probe, prints the row and folds it into the
//! overall verdict.
//! What: puts `trusty-secrets`, `codesign` and `security` shims first on the
//! child's `PATH`. `codesign` reports a new cdhash; `security` dumps one
//! vault item whose ACL pins either that cdhash or an older one.
//! Test: `cargo test -p trusty-mpm --test integration tm_doctor_secrets_signature::`.

use std::path::Path;

use crate::common;

const NEW: &str = "1111111111111111111111111111111111111111";
const OLD: &str = "2222222222222222222222222222222222222222";

fn write_shims(dir: &Path, acl_cdhash: &str) {
    let bin = dir.join("trusty-secrets");
    trusty_common::exec_stub::write_executable(&bin, "#!/bin/sh\nexit 0\n".to_string())
        .expect("write trusty-secrets shim");
    let codesign = format!("#!/bin/sh\necho 'CDHash={NEW}' >&2\nexit 0\n");
    trusty_common::exec_stub::write_executable(&dir.join("codesign"), codesign)
        .expect("write codesign shim");
    let security = format!(
        "#!/bin/sh\ncat <<'EOF'\n\
         keychain: \"/tmp/login.keychain-db\"\n\
         class: \"genp\"\n\
         attributes:\n\
         \x20   \"svce\"<blob>=\"trusty-secrets.vault-blob\"\n\
         access: 1 entries\n\
         \x20   entry 0:\n\
         \x20       applications (1):\n\
         \x20           0: {bin} (status -67068)\n\
         \x20               requirement: cdhash H\"{acl_cdhash}\"\n\
         EOF\n",
        bin = bin.display()
    );
    trusty_common::exec_stub::write_executable(&dir.join("security"), security)
        .expect("write security shim");
}

/// Run `tm doctor` with the shims first on `PATH`; return the row and stdout.
fn signature_row(acl_cdhash: &str) -> (String, String) {
    let home = tempfile::tempdir().expect("scratch home");
    let cwd = tempfile::tempdir().expect("scratch cwd");
    let shim_dir = home.path().join("shim-bin");
    std::fs::create_dir_all(&shim_dir).expect("shim dir");
    write_shims(&shim_dir, acl_cdhash);
    let path = format!(
        "{}:{}",
        shim_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = common::tm_command_in(home.path())
        .env("PATH", path)
        .arg("doctor")
        .current_dir(cwd.path())
        .output()
        .expect("failed to spawn `tm doctor`");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let row = stdout
        .lines()
        .find(|l| l.contains("secrets_signature"))
        .unwrap_or_else(|| panic!("no secrets_signature row.\nstdout:\n{stdout}"))
        .to_string();
    (row, stdout)
}

#[test]
fn tm_doctor_flags_a_trusty_secrets_signature_drift() {
    let (row, stdout) = signature_row(OLD);
    assert!(row.contains("signature drift"), "row: {row}");
    assert!(row.contains("tm secrets unlock"), "row: {row}");
    assert!(!stdout.contains("all checks passed"), "stdout:\n{stdout}");
}

#[test]
fn tm_doctor_passes_the_signature_row_when_the_acl_trusts_the_binary() {
    let (row, _) = signature_row(NEW);
    assert!(
        row.contains("1 item(s) trust cdhash 111111111111"),
        "row: {row}"
    );
}
