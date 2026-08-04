//! Tests for [`super::refresh_compiled_prompt_for_resume`] (#4752).
//!
//! Why: the resume path never runs `prepare_session*`, so this helper is the
//! ONLY thing standing between a resumed session and a stale (or absent)
//! compiled prompt. It is also fatal, so its failure message is operator-facing
//! and must be assertable.
//! What: covers the success write and the actionable-failure message.
//! Test: this file IS the test suite.

use super::*;

#[test]
fn refresh_compiled_prompt_for_resume_writes_the_project_local_file() {
    // Why (#4752 review, HIGH 3): before this helper existed, resume ran a
    // prompt that never reached the compiled path. FIXTURE: pre-seed a stale
    // sentinel so "the file exists" cannot pass — only a real refresh can.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = crate::core::instruction_pipeline::compiled_prompt_path(tmp.path());
    std::fs::create_dir_all(dest.parent().unwrap()).expect("create parent");
    const STALE: &str = "STALE-FROM-A-PREVIOUS-START";
    std::fs::write(&dest, STALE).expect("seed stale");

    refresh_compiled_prompt_for_resume(tmp.path()).expect("refresh succeeds");

    let on_disk = std::fs::read_to_string(&dest).expect("readable");
    assert_ne!(
        on_disk, STALE,
        "resume must overwrite a stale compiled prompt"
    );
    assert!(
        on_disk.contains("# PM Agent -- Trusty MPM"),
        "must hold the resolved PM prompt, not a fragment: {on_disk}"
    );
    // Project-local, never global.
    assert!(dest.starts_with(tmp.path()));
}

#[test]
fn refresh_compiled_prompt_for_resume_reports_an_actionable_failure() {
    // Why (#4752 ruling follow-up 1): the write is fatal, so a refused resume
    // must name the path and say the session did not start — not surface a bare
    // io error. FIXTURE: a directory planted at the exact path so only this
    // write fails.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = crate::core::instruction_pipeline::compiled_prompt_path(tmp.path());
    std::fs::create_dir_all(&dest).expect("plant a directory at the compiled path");

    let msg = refresh_compiled_prompt_for_resume(tmp.path())
        .expect_err("a failed compiled write must refuse the resume");
    assert!(
        msg.contains(&dest.display().to_string()),
        "must name the path: {msg}"
    );
    assert!(
        msg.contains("was NOT started"),
        "must say it did not start: {msg}"
    );
    assert!(msg.contains("permissions"), "must point at a remedy: {msg}");
}
