//! #8436 PR C: the guard-lift reader and writer against scratch projects.
//! Each plan §4 branch that ends in "FAILED, file untouched" checks the bytes.

use super::*;
use tempfile::tempdir;

/// Plant `body` as `dir`'s `.claude/settings.json`.
fn plant(dir: &Path, body: &str) {
    std::fs::create_dir_all(dir.join(".claude")).unwrap();
    std::fs::write(settings_path(dir), body).unwrap();
}

/// The settings file, parsed.
fn doc(dir: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(settings_path(dir)).unwrap()).unwrap()
}

#[test]
fn the_state_reader_covers_every_shape() {
    let dir = tempdir().unwrap();
    assert_eq!(read_state(dir.path()), LiftState::Off);
    for (body, want) in [
        ("", LiftState::Off),
        ("{}", LiftState::Off),
        ("{\"env\": {}}", LiftState::Off),
        (
            "{\"env\": {\"TRUSTY_MPM_PM_UNRESTRICTED\": \"1\"}}",
            LiftState::Lifted,
        ),
        (
            "{\"env\": {\"TRUSTY_MPM_PM_UNRESTRICTED\": 1}}",
            LiftState::Other("1".to_owned()),
        ),
        (
            "{\"env\": {\"TRUSTY_MPM_PM_UNRESTRICTED\": \"yes\"}}",
            LiftState::Other("\"yes\"".to_owned()),
        ),
    ] {
        plant(dir.path(), body);
        assert_eq!(read_state(dir.path()), want, "{body:?}");
    }
    for body in ["{ not json", "[1]", "{\"env\": 3}"] {
        plant(dir.path(), body);
        assert!(
            matches!(read_state(dir.path()), LiftState::Unknown(_)),
            "{body:?}"
        );
    }
}

#[test]
fn set_creates_merges_and_reports_the_old_value() {
    let dir = tempdir().unwrap();
    assert_eq!(set(dir.path()), Ok(LiftChange::Written { was: None }));
    assert_eq!(doc(dir.path())["env"][LIFT_KEY], "1");
    assert_eq!(set(dir.path()), Ok(LiftChange::Unchanged));

    plant(
        dir.path(),
        "{\"model\": \"opus\", \"env\": {\"FOO\": \"x\", \"TRUSTY_MPM_PM_UNRESTRICTED\": \"0\"}}",
    );
    assert_eq!(
        set(dir.path()),
        Ok(LiftChange::Written {
            was: Some("\"0\"".to_owned())
        })
    );
    let after = doc(dir.path());
    assert_eq!(after["model"], "opus");
    assert_eq!(after["env"]["FOO"], "x");
    assert_eq!(after["env"][LIFT_KEY], "1");
    let leftovers: Vec<_> = std::fs::read_dir(dir.path().join(".claude"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != "settings.json" && !n.ends_with(".lock"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn remove_drops_only_the_key() {
    let dir = tempdir().unwrap();
    assert_eq!(remove(dir.path()), Ok(LiftChange::Unchanged));
    assert!(!settings_path(dir.path()).exists());
    plant(
        dir.path(),
        "{\"env\": {\"FOO\": \"x\", \"TRUSTY_MPM_PM_UNRESTRICTED\": \"1\"}}",
    );
    assert_eq!(remove(dir.path()), Ok(LiftChange::Written { was: None }));
    let after = doc(dir.path());
    assert!(after["env"].get(LIFT_KEY).is_none(), "{after}");
    assert_eq!(after["env"]["FOO"], "x");
    assert_eq!(remove(dir.path()), Ok(LiftChange::Unchanged));
}

#[test]
fn an_unusable_file_is_refused_and_left_byte_identical() {
    for body in ["{ not json", "[1, 2]", "{\"env\": \"x\"}"] {
        let dir = tempdir().unwrap();
        plant(dir.path(), body);
        assert!(set(dir.path()).is_err(), "{body:?}");
        assert!(remove(dir.path()).is_err(), "{body:?}");
        assert_eq!(
            std::fs::read_to_string(settings_path(dir.path())).unwrap(),
            body
        );
    }
}

#[test]
fn a_symlinked_settings_file_or_claude_dir_is_refused() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("elsewhere.json");
    std::fs::write(&target, "{}").unwrap();
    std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
    std::os::unix::fs::symlink(&target, settings_path(dir.path())).unwrap();
    assert!(set(dir.path()).is_err());
    assert!(matches!(read_state(dir.path()), LiftState::Unknown(_)));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "{}");

    let dir = tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, dir.path().join(".claude")).unwrap();
    assert!(set(dir.path()).is_err());
    assert!(std::fs::read_dir(&real).unwrap().next().is_none());
}

/// Plant `body` as `dir`'s `.claude/settings.local.json`.
fn plant_local(dir: &Path, body: &str) {
    std::fs::create_dir_all(dir.join(".claude")).unwrap();
    std::fs::write(local_settings_path(dir), body).unwrap();
}

const LIFTED: &str = "{\"env\": {\"TRUSTY_MPM_PM_UNRESTRICTED\": \"1\"}}";

/// #8436 PR C (M3): Claude Code merges `settings.local.json` over
/// `settings.json`, so a lift in either file is a lift, and the state names it.
#[test]
fn a_lift_in_either_settings_file_is_a_lift() {
    let dir = tempdir().unwrap();
    plant(dir.path(), "{}");
    plant_local(dir.path(), LIFTED);
    assert_eq!(read_state(dir.path()), LiftState::Lifted);
    assert_eq!(lifted_in(dir.path()), vec!["settings.local.json"]);

    plant(dir.path(), LIFTED);
    assert_eq!(read_state(dir.path()), LiftState::Lifted);
    assert_eq!(
        lifted_in(dir.path()),
        vec!["settings.json", "settings.local.json"]
    );

    plant_local(dir.path(), "{}");
    assert_eq!(lifted_in(dir.path()), vec!["settings.json"]);
}

/// #8436 PR C (M3), Fail-Open Check: a local file that cannot be read never
/// reads as off; a known lift still reads as lifted.
#[test]
fn an_unreadable_local_file_is_unknown_never_off() {
    for body in ["{ not json", "[1]", "{\"env\": 3}"] {
        let dir = tempdir().unwrap();
        plant(dir.path(), "{}");
        plant_local(dir.path(), body);
        assert!(
            matches!(read_state(dir.path()), LiftState::Unknown(_)),
            "{body:?}"
        );
        assert!(lifted_in(dir.path()).is_empty(), "{body:?}");
        plant(dir.path(), LIFTED);
        assert_eq!(read_state(dir.path()), LiftState::Lifted, "{body:?}");
    }
    let dir = tempdir().unwrap();
    plant(dir.path(), "{}");
    plant_local(
        dir.path(),
        "{\"env\": {\"TRUSTY_MPM_PM_UNRESTRICTED\": \"yes\"}}",
    );
    assert!(matches!(read_state(dir.path()), LiftState::Other(_)));
}
