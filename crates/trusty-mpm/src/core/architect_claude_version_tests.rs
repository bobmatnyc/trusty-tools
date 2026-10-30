//! Tests for [`super`]: each refusal arm of the Architect's Claude Code
//! version gate (#10459 PR 2, DOC-86 §5).

use super::*;

#[test]
fn launch_refuses_below_minimum_version() {
    for old in ["2.1.294 (Claude Code)", "2.0.999", "1.99.400"] {
        let err = check_claude_version(Ok(old.to_owned())).expect_err(old);
        assert!(
            matches!(err, ArchitectVersionError::TooOld(_)),
            "{old}: {err:?}"
        );
        let text = err.to_string();
        assert!(text.contains("older than v2.1.295"), "{text}");
        assert!(text.contains("claude update"), "{text}");
    }
}

#[test]
fn launch_refuses_an_unreadable_version() {
    let err = check_claude_version(Err("timed out after 5s".to_owned())).expect_err("unreadable");
    assert_eq!(
        err,
        ArchitectVersionError::Unreadable("timed out after 5s".to_owned())
    );
    assert!(
        err.to_string()
            .contains("cannot read the Claude Code version")
    );
}

#[test]
fn launch_refuses_an_unparsable_version() {
    for junk in ["", "Claude Code", "2.1 (Claude Code)", "v2.x.295"] {
        let err = check_claude_version(Ok(junk.to_owned())).expect_err(junk);
        assert!(
            matches!(err, ArchitectVersionError::Unparsable(_)),
            "{junk:?}: {err:?}"
        );
    }
}

#[test]
fn launch_accepts_the_minimum_and_newer() {
    assert_eq!(
        check_claude_version(Ok("2.1.295 (Claude Code)\n".to_owned())),
        Ok((2, 1, 295))
    );
    assert_eq!(
        check_claude_version(Ok("2.1.312 (Claude Code)".to_owned())),
        Ok((2, 1, 312))
    );
    assert_eq!(check_claude_version(Ok("3.0.0".to_owned())), Ok((3, 0, 0)));
}

#[test]
fn a_pm_profile_is_never_probed() {
    // A PM launch returns before any `claude --version` spawn, on any host.
    assert_eq!(
        require_for_profile(crate::core::session_profile::SessionProfile::Pm),
        Ok(())
    );
}
