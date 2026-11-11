//! Tests for `tm fleet init`'s watch-list seed (#11012). The registry, the
//! tmux sessions and the Architect directory are fixtures; nothing reads the
//! operator's home, managed root or tmux server.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::super::super::Step;
use super::super::init_seed::{InitWatch, LiveSession, steps};
use super::{Fixture, ReadOnly, Registry};

/// The Architect's own sessions, which are never watched.
const OWN: [&str; 2] = ["tm-architect", "tm-architect-poll"];

fn live(name: &str, path: Option<PathBuf>) -> LiveSession {
    LiveSession {
        name: name.into(),
        path,
    }
}

fn source(fx: &Fixture, sessions: Option<Vec<LiveSession>>) -> InitWatch {
    InitWatch {
        registry: Ok(fx.registry()),
        sessions,
    }
}

/// `tm-alpha` (alias), `tm-beta-fork-02` (beta's alias), `tm-other` in
/// beta's checkout (path), the Architect's own, an unmapped `tm-stray`, and
/// a session without the `tm-` prefix.
fn sessions(fx: &Fixture) -> Vec<LiveSession> {
    vec![
        live("tm-alpha", Some(PathBuf::from("/elsewhere"))),
        live("tm-beta-fork-02", None),
        live("tm-other", Some(fx.checkout("beta"))),
        live("tm-architect", Some(fx.dir())),
        live("tm-stray", Some(PathBuf::from("/nowhere"))),
        live("scratch", Some(fx.checkout("alpha"))),
    ]
}

fn text(steps: &[Step]) -> String {
    steps.iter().map(Step::render_line).collect()
}

fn failed(steps: &[Step]) -> bool {
    steps.iter().any(|s| matches!(s, Step::Failed(_)))
}

#[test]
fn init_seed_writes_the_mapped_projects_and_their_records() {
    // AC1: exactly the mapped projects; the unmapped session is one line.
    let fx = Fixture::new();
    let out = steps(&fx.dir(), &source(&fx, Some(sessions(&fx))), &OWN);
    let report = text(&out);
    assert!(!failed(&out), "{report}");
    assert_eq!(fx.keys(), ["alpha", "beta"], "{report}");
    let skipped = out.iter().filter(|s| matches!(s, Step::Skipped(_))).count();
    assert_eq!(skipped, 1, "{report}");
    assert!(
        report.contains("tmux session tm-stray not watched"),
        "{report}"
    );
    assert!(!report.contains("tm-architect"), "{report}");
    assert!(
        report.contains("with 2 project(s), 2 from live tm-* sessions"),
        "{report}"
    );
    for key in ["alpha", "beta"] {
        let record = fx.dir().join(format!("local/projects/{key}.md"));
        let body = std::fs::read_to_string(&record).unwrap();
        assert!(body.contains(key), "{body}");
    }
}

#[test]
fn init_seed_rerun_leaves_the_list_byte_identical() {
    // AC2: a second run publishes nothing — same bytes, inode and mtime.
    let fx = Fixture::new();
    let watch = source(&fx, Some(sessions(&fx)));
    steps(&fx.dir(), &watch, &OWN);
    let meta = |p: &Path| std::fs::metadata(p).unwrap();
    let bytes = std::fs::read(fx.list_path()).unwrap();
    let before = meta(&fx.list_path());
    std::thread::sleep(std::time::Duration::from_millis(20));
    let again = steps(&fx.dir(), &watch, &OWN);
    assert!(
        matches!(again.as_slice(), [Step::Unchanged(t)] if t.contains("exists with 2 project(s)")),
        "{}",
        text(&again)
    );
    let after = meta(&fx.list_path());
    assert_eq!(std::fs::read(fx.list_path()).unwrap(), bytes);
    assert_eq!(after.ino(), before.ino(), "the list was republished");
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
}

#[test]
fn init_seed_never_edits_an_owner_changed_list() {
    // AC3: an emptied list and an owner-edited list keep their entries.
    let fx = Fixture::new();
    let watch = source(&fx, Some(sessions(&fx)));
    for keys in [&[][..], &["beta"][..], &["gone"][..]] {
        fx.write_list(keys);
        let bytes = std::fs::read(fx.list_path()).unwrap();
        let out = steps(&fx.dir(), &watch, &OWN);
        assert!(
            matches!(out.as_slice(), [Step::Unchanged(_)]),
            "{}",
            text(&out)
        );
        assert_eq!(std::fs::read(fx.list_path()).unwrap(), bytes);
    }
}

#[test]
fn init_seed_malformed_list_fails_and_is_left_unchanged() {
    // Fail-closed: a list that does not parse is never taken for absent.
    let fx = Fixture::new();
    for bad in ["{not json", r#"{"version": 2, "projects": []}"#] {
        std::fs::write(fx.list_path(), bad).unwrap();
        let out = steps(&fx.dir(), &source(&fx, Some(sessions(&fx))), &OWN);
        let report = text(&out);
        assert!(failed(&out), "{report}");
        let named = fx.list_path().display().to_string();
        assert!(report.contains(&named), "{report}");
        assert_eq!(std::fs::read_to_string(fx.list_path()).unwrap(), bad);
        assert!(!fx.dir().join("local/projects").exists(), "{report}");
    }
}

#[test]
fn init_seed_write_failure_is_a_failed_step() {
    // AC4: a record that cannot be written leaves no list; so does a list
    // directory that cannot be written.
    let fx = Fixture::new();
    let watch = source(&fx, Some(sessions(&fx)));
    let projects = fx.dir().join("local/projects");
    std::fs::create_dir_all(&projects).unwrap();
    let guard = ReadOnly::new(&projects);
    let out = steps(&fx.dir(), &watch, &OWN);
    drop(guard);
    let report = text(&out);
    assert!(failed(&out), "{report}");
    assert!(
        report.contains("not written") && report.contains("cannot write the record"),
        "{report}"
    );
    assert!(!fx.list_path().exists(), "{report}");
    let guard = ReadOnly::new(&fx.dir().join("local"));
    let out = steps(&fx.dir(), &watch, &OWN);
    drop(guard);
    assert!(failed(&out), "{}", text(&out));
    assert!(!fx.list_path().exists());
}

#[test]
fn init_seed_without_tmux_writes_an_empty_list() {
    // No tmux, or no `tm-*` session: an empty list and a line saying why.
    for (sessions, why) in [
        (None, "tmux could not be read"),
        (Some(Vec::new()), "no live tm-* session maps"),
    ] {
        let fx = Fixture::new();
        let out = steps(&fx.dir(), &source(&fx, sessions), &OWN);
        assert!(
            matches!(out.as_slice(), [Step::Changed(t)] if t.contains(why)),
            "{}",
            text(&out)
        );
        assert!(fx.keys().is_empty());
    }
}

#[test]
fn init_seed_keeps_the_claude_md_import_keys() {
    // An upgraded Architect's hand-written lines survive the seed.
    let fx = Fixture::new();
    let beta = fx.checkout("beta").display().to_string();
    fx.write_claude_md(&[&format!("- `tm-beta`: `{beta}`"), "- `tm-zeta`: `/zeta`"]);
    let watch = source(&fx, Some(vec![live("tm-alpha", None)]));
    let out = steps(&fx.dir(), &watch, &OWN);
    let report = text(&out);
    assert_eq!(fx.keys(), ["alpha", "beta"], "{report}");
    assert!(
        report.contains("skipped") && report.contains("tm-zeta"),
        "{report}"
    );
}

#[tokio::test]
async fn init_seed_without_a_registry_writes_nothing() {
    // An absent registry maps nothing: skipped, so a later run can seed. A
    // registry that did not load is a FAILED step.
    let fx = Fixture::new();
    let empty = Registry::load(&fx.tmp.path().join("empty")).await.unwrap();
    let watch = InitWatch {
        registry: Ok(empty),
        sessions: Some(sessions(&fx)),
    };
    let out = steps(&fx.dir(), &watch, &OWN);
    assert!(
        matches!(out.as_slice(), [Step::Skipped(t)] if t.contains("TRUSTY_MPM_ROOT")),
        "{}",
        text(&out)
    );
    assert!(!fx.list_path().exists());
    let broken = InitWatch {
        registry: Err(anyhow::anyhow!("projects.json is malformed")),
        sessions: None,
    };
    let out = steps(&fx.dir(), &broken, &OWN);
    assert!(failed(&out), "{}", text(&out));
    assert!(!fx.list_path().exists());
}

#[test]
fn init_seed_runs_inside_init_before_the_launch() {
    // AC5: an Architect set up without a list gets one from a re-run of
    // `init`; a second re-run changes nothing.
    use trusty_mpm::core::content_source::Fetch;
    use trusty_mpm::core::project_skill_tier::architect_seed::ArchitectSeed;
    let fx = Fixture::new();
    let home = fx.tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let dir = fx.dir();
    let checkout = trusty_common::test_harness::test_repo_root().expect("the checkout");
    let run = |watch: Option<&InitWatch>| {
        super::super::super::init_full(
            &dir,
            &home,
            false,
            super::super::super::tests::NO_TMUX,
            None,
            |_| ArchitectSeed::resolve_in(&checkout, None, None, Fetch::Never),
            watch,
        )
        .unwrap()
    };
    run(None);
    assert!(!fx.list_path().exists(), "a pre-#11012 install has no list");
    let watch = source(&fx, Some(sessions(&fx)));
    let report = run(Some(&watch)).render();
    assert!(report.contains("wrote watch list"), "{report}");
    assert_eq!(fx.keys(), ["alpha", "beta"]);
    let bytes = std::fs::read(fx.list_path()).unwrap();
    let report = run(Some(&watch)).render();
    assert!(report.contains("init leaves it as is"), "{report}");
    assert_eq!(std::fs::read(fx.list_path()).unwrap(), bytes);
}
