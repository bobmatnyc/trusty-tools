//! #9171 A2: the live prototype's `tests/test_quiet_sessions.py`, ported by
//! name against `trusty_architect::quiet`.

use std::collections::BTreeSet;

use trusty_architect::quiet::{is_test_fixture_session, read_quiet_sessions};

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

// --- read_quiet_sessions ------------------------------------------------------

#[test]
fn test_read_quiet_sessions_one_per_line_with_comments_and_blanks() -> std::io::Result<()> {
    let dir = tempfile::tempdir()?;
    let f = dir.path().join("quiet-sessions.txt");
    std::fs::write(
        &f,
        "# parked by the owner\ntm-hotstats-reporting-01\n\ntm-ai-power-rankings-01  # paused pending #8261\n   \n",
    )?;
    assert_eq!(
        read_quiet_sessions(&f),
        set(&["tm-hotstats-reporting-01", "tm-ai-power-rankings-01"])
    );
    Ok(())
}

#[test]
fn test_read_quiet_sessions_missing_file_is_an_empty_set() -> std::io::Result<()> {
    let dir = tempfile::tempdir()?;
    assert!(read_quiet_sessions(&dir.path().join("does-not-exist.txt")).is_empty());
    Ok(())
}

#[test]
fn test_read_quiet_sessions_all_comments_or_blank_is_an_empty_set() -> std::io::Result<()> {
    let dir = tempfile::tempdir()?;
    let f = dir.path().join("quiet-sessions.txt");
    std::fs::write(&f, "# nothing listed yet\n\n")?;
    assert!(read_quiet_sessions(&f).is_empty());
    Ok(())
}

#[test]
fn test_read_quiet_sessions_reflects_the_current_file_each_call() -> std::io::Result<()> {
    let dir = tempfile::tempdir()?;
    let f = dir.path().join("quiet-sessions.txt");
    std::fs::write(&f, "tm-a\n")?;
    assert_eq!(read_quiet_sessions(&f), set(&["tm-a"]));
    std::fs::write(&f, "tm-a\ntm-b\n")?;
    assert_eq!(read_quiet_sessions(&f), set(&["tm-a", "tm-b"]));
    Ok(())
}

// --- is_test_fixture_session --------------------------------------------------

#[test]
fn test_test_fixture_globs_match() {
    for name in [
        "tm-xtest-1",
        "tm-xtest-anything",
        "tm-qa1-run",
        "tm-qa-x",
        "tm-r0",
        "tm-r42-worker",
    ] {
        assert!(is_test_fixture_session(name), "{name}");
    }
}

#[test]
fn test_test_fixture_globs_do_not_match_real_sessions() {
    for name in [
        "tm-supervisor",
        "tm-hotstats-reporting-01",
        "tm-api",
        "tm-qa",
        "tm-r",
        "tm-rfoo",
        "tm-xtest",
    ] {
        assert!(!is_test_fixture_session(name), "{name}");
    }
}
