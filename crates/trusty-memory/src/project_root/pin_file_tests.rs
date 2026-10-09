//! Writer-side tests for the pin schema rule (#9274, ADR-0067 D2).
//!
//! Why: `write_project_pin` is the only pin writer. Each test below failed on
//! the pre-fix writer, which overwrote whatever file it found and serialised
//! only the fields it knew.

use super::*;
use std::fs;

/// Write `raw` as the pin file under `root`, creating `.trusty-tools/`.
fn write_raw_pin(root: &Path, raw: &str) -> PathBuf {
    fs::create_dir_all(root.join(TRUSTY_TOOLS_DIR)).expect("create .trusty-tools");
    let path = root.join(PIN_FILE_REL);
    fs::write(&path, raw).expect("write raw pin");
    path
}

/// Why: a newer pin must be left exactly as it is, and the error must say which
/// file and which versions, so the operator knows to upgrade.
#[test]
fn write_refuses_to_overwrite_a_newer_pin() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let raw = "schema_version: 2\npalace: future-name\nproject_uuid: 0f3c\n";
    let path = write_raw_pin(tmp.path(), raw);

    let err = write_project_pin(tmp.path(), &ProjectPin::new("older-name"))
        .expect_err("a newer pin must not be overwritten");

    assert_eq!(fs::read_to_string(&path).expect("read"), raw);
    let msg = format!("{err:#}");
    let path_text = path.display().to_string();
    for needle in [path_text.as_str(), "schema_version 2", "schema_version 1"] {
        assert!(msg.contains(needle), "`{msg}` must name `{needle}`");
    }
}

/// Why: `link --force` builds a fresh pin, and the rewrite must not drop the
/// fields a later release added to the file it replaces.
#[test]
fn rewrite_keeps_the_unknown_fields_of_the_pin_on_disk() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = write_raw_pin(
        tmp.path(),
        "schema_version: 1\npalace: old-name\nproject_uuid: 0f3c\nsync:\n  enabled: true\n",
    );

    write_project_pin(tmp.path(), &ProjectPin::new("new-name")).expect("write ok");

    let written: serde_yaml::Value =
        serde_yaml::from_str(&fs::read_to_string(&path).expect("read")).expect("parse");
    assert_eq!(written["palace"].as_str(), Some("new-name"));
    assert_eq!(written["project_uuid"].as_str(), Some("0f3c"));
    assert_eq!(written["sync"]["enabled"].as_bool(), Some(true));
}

/// Why (fail-open check): a pin the writer cannot read may be a newer one, so
/// the read error must stop the write rather than be treated as "no pin".
#[cfg(unix)]
#[test]
fn write_refuses_when_the_pin_on_disk_cannot_be_read() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().expect("tempdir");
    let raw = "schema_version: 1\npalace: kept-name\n";
    let path = write_raw_pin(tmp.path(), raw);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("chmod 000");
    if fs::read(&path).is_ok() {
        // Running as root: permissions do not block the read, so the case
        // this test needs cannot be built.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod back");
        return;
    }

    let result = write_project_pin(tmp.path(), &ProjectPin::new("other-name"));

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod back");
    let err = result.expect_err("an unreadable pin must not be overwritten");
    assert!(
        format!("{err:#}").contains(&path.display().to_string()),
        "the error must name the pin file: {err:#}"
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), raw);
}
