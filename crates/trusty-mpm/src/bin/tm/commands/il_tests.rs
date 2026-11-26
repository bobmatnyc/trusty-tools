//! Tests for `tm il config`: parsing and the CLI path over a temp Architect dir.

use clap::Parser;
use trusty_mpm::core::il_schedule::{self, PRESET};

use super::run;
use crate::cli::{Cli, Command, IlAction, IlConfigAction};

/// Parse `tm il config <argv>`.
fn parse(argv: &[&str]) -> (Option<String>, IlConfigAction) {
    let full = ["tm", "il", "config"].iter().chain(argv).copied();
    match Cli::try_parse_from(full).expect("parses").command {
        Some(Command::Il {
            action: IlAction::Config { dir, action },
        }) => (dir, action),
        other => panic!("{argv:?} parsed as {other:?}"),
    }
}

#[test]
fn cli_parses_every_il_config_verb() {
    assert!(matches!(parse(&["show"]), (None, IlConfigAction::Show)));
    let (dir, enroll) = parse(&[
        "enroll",
        "tm-a",
        "--project",
        "/p",
        "--code",
        "ab",
        "--dir",
        "/d",
    ]);
    assert_eq!(dir.as_deref(), Some("/d"));
    assert!(
        matches!(enroll, IlConfigAction::Enroll { ref session, code: Some(_), .. }
            if session == "tm-a")
    );
    assert!(matches!(
        parse(&["remove", "tm-a"]).1,
        IlConfigAction::Remove { .. }
    ));
    let (_, set) = parse(&["set", "--session", "tm-a", "thresholds.floor_ctx", "1"]);
    assert!(matches!(
        set,
        IlConfigAction::Set {
            session: Some(_),
            ..
        }
    ));
    // #11002: the preset diff and the migration take no arguments.
    assert!(matches!(parse(&["diff"]).1, IlConfigAction::Diff));
    assert!(matches!(parse(&["update"]).1, IlConfigAction::Update));
    assert!(matches!(
        parse(&["reset", "--force"]).1,
        IlConfigAction::Reset { force: true }
    ));
    assert!(
        Cli::try_parse_from(["tm", "il", "config", "enroll", "tm-a"]).is_err(),
        "--project is required"
    );
}

/// The CLI path writes through the library and refuses with an error.
#[test]
fn the_cli_enrolls_sets_and_resets_in_the_given_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().to_str().expect("utf8").to_owned();
    let project = tmp.path().join("proj");
    std::fs::create_dir(&project).expect("proj");
    let config = |action| IlAction::Config {
        dir: Some(dir.clone()),
        action,
    };
    run(config(IlConfigAction::Enroll {
        session: "tm-a".into(),
        project: project.clone(),
        code: None,
    }))
    .expect("enroll");
    run(config(IlConfigAction::Set {
        key: "thresholds.floor_ctx".into(),
        value: "x".into(),
        session: None,
    }))
    .expect_err("a bad value is an error");
    let file = il_schedule::path(tmp.path());
    assert!(
        std::fs::read_to_string(&file)
            .expect("file")
            .contains("tm-a")
    );
    run(config(IlConfigAction::Reset { force: false })).expect_err("edited file needs --force");
    run(config(IlConfigAction::Reset { force: true })).expect("reset");
    assert_eq!(std::fs::read_to_string(&file).expect("file"), PRESET);
}

#[test]
fn the_cli_refuses_a_missing_architect_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("none").to_str().expect("utf8").to_owned();
    let err = run(IlAction::Config {
        dir: Some(missing),
        action: IlConfigAction::Show,
    })
    .expect_err("no dir");
    assert!(err.to_string().contains("tm fleet init"), "{err}");
}

/// #11002: `diff` reads only, and `update` migrates an older file in place.
#[test]
fn the_cli_diffs_and_updates_an_older_schedule() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().to_str().expect("utf8").to_owned();
    let file = il_schedule::path(tmp.path());
    std::fs::create_dir_all(file.parent().expect("local")).expect("local");
    let v0 = PRESET.replacen("  \"schema_version\": 1,\n", "", 1);
    std::fs::write(&file, &v0).expect("seed an unversioned file");
    let config = |action| IlAction::Config {
        dir: Some(dir.clone()),
        action,
    };
    run(config(IlConfigAction::Diff)).expect("diff");
    assert_eq!(
        std::fs::read_to_string(&file).expect("file"),
        v0,
        "diff wrote"
    );
    run(config(IlConfigAction::Update)).expect("update");
    let loaded = il_schedule::load(&file).expect("load");
    assert_eq!(
        loaded.schedule.schema_version,
        Some(il_schedule::SCHEMA_VERSION)
    );
}
