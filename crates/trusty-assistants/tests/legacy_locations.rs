//! The binary moves the pre-rename directories before it reads state (#11632).
//!
//! Why: the unit tests in `assistants::tests::relocate_tests` cover the move;
//! this proves the shipped binary runs it, and reads data that was written
//! under the old names, on a real command.
//! What: seeds `~/.trusty-agents/user.toml` and `~/trusty-agents/izzie/` in
//! an isolated `$HOME`, runs `ta config profile show`, and checks the
//! profile was read, the move was reported, and the data sits under the new
//! names.
//! Test: this file IS the test.

use std::process::Command;

/// Why: `config profile show` reads `user.toml` from the app directory, so
/// it is the smallest real command that loads moved state.
/// Test: itself.
#[test]
fn ta_moves_the_old_directories_then_reads_them() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    std::fs::create_dir_all(home.join(".trusty-agents")).unwrap();
    std::fs::write(
        home.join(".trusty-agents/user.toml"),
        "name = \"Ada Pilot\"\ncreated_at = \"2026-01-01T00:00:00Z\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(home.join("trusty-agents/izzie")).unwrap();
    std::fs::write(home.join("trusty-agents/izzie/instructions.md"), "izzie").unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ta"));
    trusty_common::parent_death::exit_with_parent(&mut cmd);
    let out = cmd
        .args(["config", "profile", "show"])
        .current_dir(home)
        .env("HOME", home)
        .env_remove("TA_ASSISTANTS_DIR")
        .env_remove("TAGENT_ASSISTANTS_DIR")
        .env_remove("TAGENT_USER_NAME")
        .output()
        .expect("spawn ta");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr:\n{stderr}");
    assert!(stdout.contains("Ada Pilot"), "stdout:\n{stdout}");
    assert!(stderr.contains("moved"), "stderr:\n{stderr}");
    assert!(home.join(".trusty-assistants/user.toml").is_file());
    assert_eq!(
        std::fs::read_to_string(home.join("trusty-assistants/izzie/instructions.md")).unwrap(),
        "izzie"
    );
}
