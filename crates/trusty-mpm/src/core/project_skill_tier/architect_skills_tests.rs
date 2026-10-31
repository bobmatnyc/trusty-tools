//! #9540: the Architect skill deployer against a scratch project tier.
//!
//! Why: df-D22's rules — write when absent, refresh only an untouched copy,
//! never overwrite a project-custom one, never fail the caller — each need a
//! case that a wrong deployer fails.
//! What: drives [`super::deploy_skills_into`] and [`super::health`] on temp
//! dirs; the launch-path cases live in
//! `session_launch::skills::architect_9540_tests`.
//! Test: this is the test module.

use super::*;
use crate::core::project_skill_tier::architect_seed::test_support::shipped;
use std::time::{Duration, SystemTime};
use tempfile::tempdir;

/// A shipped table of one skill with `contents`.
fn one(contents: &'static str) -> [ArchitectSkill; 1] {
    one_with_history(contents, &[])
}

/// [`one`] with the sha256 of earlier shipped versions.
fn one_with_history(
    contents: &'static str,
    previous: &'static [&'static str],
) -> [ArchitectSkill; 1] {
    [ArchitectSkill {
        stem: "tm-probe",
        contents: contents.to_owned(),
        previous,
    }]
}

/// A one-entry history holding the sha256 of `old_body`.
fn history_of(old_body: &str) -> &'static [&'static str] {
    let hash: &'static str = Box::leak(sha256_hex(old_body.as_bytes()).into_boxed_str());
    Box::leak(vec![hash].into_boxed_slice())
}

/// Pin `path`'s mtime to a fixed past instant and return it.
fn age(path: &Path) -> SystemTime {
    let then = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(then)
        .unwrap();
    then
}

fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

/// Every [`SHIPPED`] stem, in deploy order.
fn shipped_stems() -> Vec<&'static str> {
    SHIPPED.iter().map(|s| s.stem).collect()
}

#[test]
fn an_absent_skill_is_written_byte_equal() {
    let project = tempdir().unwrap();
    let report = deploy_architect_skills(project.path(), &[], &shipped());
    for skill in shipped() {
        let path = skill_path(&project_tier(project.path()), skill.stem);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            skill.contents.as_bytes(),
            "{}",
            path.display()
        );
    }
    assert_eq!(report.written, shipped_stems());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}

#[test]
fn an_edited_copy_is_kept_and_reported() {
    use tracing_subscriber::layer::SubscriberExt;

    let project = tempdir().unwrap();
    deploy_architect_skills(project.path(), &[], &shipped());
    let path = skill_path(&project_tier(project.path()), FLEET_CHECK.stem);
    std::fs::write(&path, "PROJECT EDIT\n").unwrap();

    crate::test_support::enable_event_capture();
    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let report = tracing::subscriber::with_default(subscriber, || {
        deploy_architect_skills(project.path(), &[], &shipped())
    });

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "PROJECT EDIT\n");
    assert_eq!(report.kept_custom, vec!["tm-fleet-check"]);
    assert!(report.written.is_empty(), "{:?}", report.written);
    let lines = buffer.tail(64);
    assert!(
        lines.iter().any(|l| l.contains("WARN")
            && l.contains("tm-fleet-check")
            && l.contains("project-custom")),
        "no WARN line names the kept copy: {lines:#?}"
    );
    let (ok, detail) = health(project.path());
    assert!(!ok, "{detail}");
    assert!(
        detail.contains("tm-fleet-check: project-custom"),
        "{detail}"
    );
    assert!(!detail.contains("tm-context-refresh"), "{detail}");
}

#[test]
fn an_untouched_copy_is_refreshed_when_the_shipped_bytes_move() {
    let dir = tempdir().unwrap();
    deploy_skills_into(dir.path(), &one("v1\n"));
    let report = deploy_skills_into(dir.path(), &one("v2\n"));
    assert_eq!(report.written, vec!["tm-probe"]);
    assert_eq!(
        std::fs::read_to_string(skill_path(dir.path(), "tm-probe")).unwrap(),
        "v2\n"
    );
}

#[test]
fn a_seeded_copy_with_no_record_and_other_bytes_is_custom() {
    // A `tm fleet init` seed that predates the record and was edited since.
    let dir = tempdir().unwrap();
    let path = skill_path(dir.path(), "tm-probe");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "older seed\n").unwrap();
    let report = deploy_skills_into(dir.path(), &one("v2\n"));
    assert_eq!(report.kept_custom, vec!["tm-probe"]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "older seed\n");
}

#[test]
fn a_byte_equal_seed_is_adopted_then_refreshed() {
    // An unrecorded copy equal to the shipped one is adopted, not rewritten,
    // and is then refreshed like any copy tm deployed.
    let dir = tempdir().unwrap();
    let path = skill_path(dir.path(), "tm-probe");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "v1\n").unwrap();
    let pinned = age(&path);
    let adopted = deploy_skills_into(dir.path(), &one("v1\n"));
    assert!(adopted.written.is_empty(), "{:?}", adopted.written);
    assert_eq!(mtime(&path), pinned);
    let refreshed = deploy_skills_into(dir.path(), &one("v2\n"));
    assert_eq!(refreshed.written, vec!["tm-probe"]);
}

#[test]
fn a_second_run_writes_nothing() {
    let project = tempdir().unwrap();
    let tier = project_tier(project.path());
    let first = deploy_architect_skills(project.path(), &[], &shipped());
    assert_eq!(first.written.len(), SHIPPED.len());
    let mut files: Vec<PathBuf> = SHIPPED.iter().map(|s| skill_path(&tier, s.stem)).collect();
    files.push(tier.join(RECORD_FILE));
    let pinned: Vec<SystemTime> = files.iter().map(|f| age(f)).collect();

    let second = deploy_architect_skills(project.path(), &[], &shipped());

    assert_eq!(second, DeployReport::default());
    let after: Vec<SystemTime> = files.iter().map(|f| mtime(f)).collect();
    assert_eq!(after, pinned, "a run with nothing changed rewrote a file");
}

#[test]
fn a_blocked_skill_dir_is_an_error_line() {
    // A FILE where the skill's directory must go: unwritable even as root.
    let project = tempdir().unwrap();
    let tier = project_tier(project.path());
    std::fs::create_dir_all(&tier).unwrap();
    std::fs::write(tier.join(FLEET_CHECK.stem), "not a dir").unwrap();

    let report = deploy_architect_skills(project.path(), &[], &shipped());

    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(
        report.errors[0].contains("tm-fleet-check"),
        "{:?}",
        report.errors
    );
    // #9997: every other shipped skill is still written.
    let others: Vec<&str> = shipped_stems()
        .into_iter()
        .filter(|stem| *stem != FLEET_CHECK.stem)
        .collect();
    assert_eq!(report.written, others);
    let (ok, detail) = health(project.path());
    assert!(!ok, "a failed deploy reported healthy: {detail}");
    assert!(detail.contains("tm-fleet-check: unreadable"), "{detail}");
}

#[test]
fn health_names_each_skill_that_is_not_current() {
    let project = tempdir().unwrap();
    let (ok, detail) = health(project.path());
    assert!(!ok);
    assert!(detail.contains("tm-fleet-check: missing"), "{detail}");
    assert!(detail.contains("tm-context-refresh: missing"), "{detail}");

    deploy_architect_skills(project.path(), &[], &shipped());
    let (ok, detail) = health(project.path());
    assert!(ok, "{detail}");
}

/// Seed `body` as the probe skill's live copy.
fn seed_probe(dir: &Path, body: &str) -> PathBuf {
    let path = skill_path(dir, "tm-probe");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn a_prior_shipped_seed_with_no_record_is_refreshed_and_recorded() {
    // #9540: a `tm fleet init` seed of an older shipped version has no record,
    // but it is a tm deploy nobody edited.
    let dir = tempdir().unwrap();
    let history = history_of("v0\n");
    let path = seed_probe(dir.path(), "v0\n");

    let report = deploy_skills_into(dir.path(), &one_with_history("v2\n", history));

    assert_eq!(report.written, vec!["tm-probe"], "{report:?}");
    assert!(report.kept_custom.is_empty(), "{:?}", report.kept_custom);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "v2\n");
    let again = deploy_skills_into(dir.path(), &one_with_history("v2\n", history));
    assert_eq!(
        again,
        DeployReport::default(),
        "the refresh was not recorded"
    );
}

#[test]
fn other_bytes_with_no_record_stay_custom_even_with_a_history() {
    let dir = tempdir().unwrap();
    let history = history_of("v0\n");
    let path = seed_probe(dir.path(), "v0\nplus a project line\n");

    let report = deploy_skills_into(dir.path(), &one_with_history("v2\n", history));

    assert_eq!(report.kept_custom, vec!["tm-probe"]);
    assert!(report.written.is_empty(), "{:?}", report.written);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "v0\nplus a project line\n"
    );
}

#[test]
fn the_shipped_history_is_pinned_so_an_asset_edit_trips() {
    // Drift guard: editing an asset without moving its old hash into
    // `previous` would make a `tm fleet init` seed of the old text look custom.
    const PINNED: [(&str, &str); 17] = [
        (
            "tm-fleet-check",
            "a2f84da744e5fd37c10a87076779c2f56ecac4554df4c2064e4748e1bd54535a",
        ),
        (
            "tm-context-refresh",
            "6b0fdc7b311e3272b706a410ab9b6f0f5608623eda9778d8577e1e37452ff014",
        ),
        (
            "architect-tmux-ops",
            "e38c80da0581139ce727e1650cf6404594b2172de0e32077a7cea17956b02382",
        ),
        (
            "architect-operating-rules",
            "821edcc7c292dc6263ddaa2538bee3e235155679d5f3282c9823b9de1ceb0109",
        ),
        (
            "architect-workflow",
            "15e6c3d5bff69fccb7066adbe228ea7376f435ea91a6fe826dad3a24f5e2c2e4",
        ),
        (
            "architect-search-memory",
            "dc52bbe51a9f7544b2f2f78c5b4e0834e4387078926cd9ba1ae715e986aeb303",
        ),
        (
            "architect-github",
            "0a839e6671261dd4999cc65c4f90ab2e782db7f44aecdeab18174ac913c68b3d",
        ),
        (
            "architect-strategy",
            "c09c629ca5e93fa6c540955caa7f399a28c905d9356e0366628d55feed62ecbf",
        ),
        (
            "architect-completion-standard",
            "5e8aac79b0d9b187a46418d0e9443188e439474b40fed535b71379ceeeb34374",
        ),
        (
            "architect-owner-model",
            "f9d9b4afd2f28fd93d45947bc0b13376a7d99f44befd73e08132136fb70f4bf1",
        ),
        (
            "architect-drift-check",
            "26a86b0a4ac74bdc255e7dd5c42e5a818b0cb776ccd3f513193f30ec5a400401",
        ),
        (
            "architect-review",
            "ce86d88ac5823fb981cb71df0e503c21377030b89911de991f84673c89fba117",
        ),
        (
            "architect-project-provision",
            "954f6c37e6c800af8819e2153e803ad62d6e29e963139410dab21ee8f473cd1f",
        ),
        (
            "architect-macos",
            "442541a32d8c81f2cdcc4d9e74f0fc3e993ae535f5d94ca9f3223af48487077b",
        ),
        (
            "architect-improvements",
            "b37627e2c31c3118884dd5a5e46cbbde5494d9b8206eb17a1c521a53883b5416",
        ),
        (
            "architect-host-admin",
            "e9c576578a627e40d5d2b5259970397fab293289c4526d29716f2f1706b3090e",
        ),
        (
            "architect-dashboard",
            "e08b26d58e8e9c39a43fa71752ec8b191a795a786708f3ce002103b975956e82",
        ),
    ];
    // #9997: a skill on its first release has no earlier text; its first
    // edit moves it off this list and its old hash into `previous`.
    const FIRST_RELEASE: [&str; 9] = [
        "architect-tmux-ops",
        "architect-search-memory",
        "architect-github",
        "architect-completion-standard",
        "architect-owner-model",
        "architect-drift-check",
        "architect-review",
        "architect-macos",
        "architect-host-admin",
    ];
    assert_eq!(PINNED.len(), SHIPPED.len());
    for (skill, (stem, pinned)) in shipped().iter().zip(PINNED) {
        assert_eq!(skill.stem, stem);
        assert_eq!(
            sha256_hex(skill.contents.as_bytes()),
            pinned,
            "{stem} changed: add the OLD hash (the PINNED value) to its `previous` \
             list, then update PINNED"
        );
        assert_eq!(
            skill.previous.is_empty(),
            FIRST_RELEASE.contains(&stem),
            "{stem}: only a first-release skill has no shipped history"
        );
        assert!(
            !skill.previous.contains(&pinned),
            "{stem}: the current hash is listed as previous"
        );
    }
}

/// The `records/`-era text of each shipped skill, as `tm fleet init` seeded
/// it before #9997.
const RECORDS_ERA: [(&str, &str); 2] = [
    (
        "tm-fleet-check",
        include_str!("testdata/9997/tm-fleet-check.md"),
    ),
    (
        "tm-context-refresh",
        include_str!("testdata/9997/tm-context-refresh.md"),
    ),
];

/// #9997 upgrade path: an untouched `records/`-era seed with no deploy record
/// is a `previous` hit and is refreshed to the `local/` text; the same seed
/// with one hand-added line stays project-custom.
#[test]
fn a_records_era_seed_is_refreshed_and_an_edited_one_stays_custom() {
    // #9997 U1b: only the two skills that shipped in the records era have a
    // records-era text; the later skills are not part of this upgrade path.
    let skills: Vec<_> = shipped()
        .into_iter()
        .filter(|s| RECORDS_ERA.iter().any(|(stem, _)| *stem == s.stem))
        .collect();
    let era_stems: Vec<&str> = RECORDS_ERA.iter().map(|(stem, _)| *stem).collect();
    assert_eq!(skills.iter().map(|s| s.stem).collect::<Vec<_>>(), era_stems);
    let untouched = tempdir().unwrap();
    for (stem, text) in RECORDS_ERA {
        let path = skill_path(untouched.path(), stem);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
    }
    let report = deploy_skills_into(untouched.path(), &skills);
    assert_eq!(report.written, era_stems, "{report:?}");
    assert!(report.kept_custom.is_empty(), "{report:?}");
    for skill in &skills {
        let live = std::fs::read_to_string(skill_path(untouched.path(), skill.stem)).unwrap();
        assert_eq!(live, skill.contents, "{}", skill.stem);
        assert!(live.contains("local/state.md"), "{}", skill.stem);
    }

    let edited = tempdir().unwrap();
    for (stem, text) in RECORDS_ERA {
        let path = skill_path(edited.path(), stem);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{text}\nA project line.\n")).unwrap();
    }
    let report = deploy_skills_into(edited.path(), &skills);
    assert_eq!(report.kept_custom, era_stems, "{report:?}");
    assert!(report.written.is_empty(), "{report:?}");
    for (stem, text) in RECORDS_ERA {
        assert_eq!(
            std::fs::read_to_string(skill_path(edited.path(), stem)).unwrap(),
            format!("{text}\nA project line.\n")
        );
    }
}

#[test]
fn a_malformed_record_keeps_a_differing_copy_and_names_the_record() {
    let dir = tempdir().unwrap();
    let path = seed_probe(dir.path(), "project edit\n");
    std::fs::write(dir.path().join(RECORD_FILE), "{ not json").unwrap();

    let report = deploy_skills_into(dir.path(), &one("v2\n"));

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "project edit\n");
    assert_eq!(report.kept_custom, vec!["tm-probe"]);
    assert!(report.written.is_empty(), "{:?}", report.written);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("malformed") && e.contains(RECORD_FILE)),
        "{:?}",
        report.errors
    );
}

#[test]
fn the_skills_row_names_a_malformed_record() {
    let project = tempdir().unwrap();
    deploy_architect_skills(project.path(), &[], &shipped());
    std::fs::write(project_tier(project.path()).join(RECORD_FILE), "{ not json").unwrap();

    let (ok, detail) = health(project.path());

    assert!(!ok, "{detail}");
    assert!(
        detail.contains("malformed") && detail.contains(RECORD_FILE),
        "{detail}"
    );
}

#[test]
fn a_record_write_failure_is_an_error_and_the_row_does_not_say_current() {
    // A directory where the record file goes: the write cannot succeed.
    let project = tempdir().unwrap();
    std::fs::create_dir_all(project_tier(project.path()).join(RECORD_FILE)).unwrap();

    let report = deploy_architect_skills(project.path(), &[], &shipped());

    assert!(
        report.errors.iter().any(|e| e.contains(RECORD_FILE)),
        "{:?}",
        report.errors
    );
    let (ok, detail) = health(project.path());
    assert!(!ok, "an unrecorded deploy read as current: {detail}");
    assert!(detail.contains(RECORD_FILE), "{detail}");
}

#[cfg(unix)]
#[test]
fn a_tier_resolving_onto_a_reserved_dir_is_refused() {
    use crate::core::paths::FrameworkPaths;

    let home = tempdir().unwrap();
    let root = home.path().join(".trusty-mpm");
    let fw = FrameworkPaths::from_root(&root);
    for reserved in [fw.claude_skills_dir(), fw.skill_deploy_dir()] {
        let project = tempdir().unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&reserved).unwrap();
        std::fs::write(
            root.join("config.toml"),
            format!(
                "[supervisor]\nprojects = [{:?}]\n",
                project.path().display().to_string()
            ),
        )
        .unwrap();
        std::fs::write(
            project.path().join(".trusty-mpm.toml"),
            "profile = \"supervisor\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(project.path().join(".claude")).unwrap();
        std::os::unix::fs::symlink(&reserved, project_tier(project.path())).unwrap();

        let report = deploy_for_launch(&root, project.path()).expect("an Architect launch");

        assert!(report.written.is_empty(), "{:?}", report.written);
        assert!(
            !report.errors.is_empty(),
            "no error line for {}",
            reserved.display()
        );
        for skill in SHIPPED {
            let landed = skill_path(&reserved, skill.stem);
            assert!(
                !landed.exists(),
                "wrote onto the reserved tier: {}",
                landed.display()
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn the_skills_row_names_a_refused_reserved_tier() {
    let project = tempdir().unwrap();
    let reserved = tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".claude")).unwrap();
    std::os::unix::fs::symlink(reserved.path(), project_tier(project.path())).unwrap();

    let (ok, detail) = health_with(project.path(), &[reserved.path().to_path_buf()], &shipped());
    assert!(!ok, "{detail}");
    assert!(detail.contains("deploy refused"), "{detail}");
    let report =
        deploy_architect_skills(project.path(), &[reserved.path().to_path_buf()], &shipped());
    assert!(report.written.is_empty(), "{:?}", report.written);
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(
        std::fs::read_dir(reserved.path()).unwrap().next().is_none(),
        "the reserved dir was written"
    );
}

#[test]
fn architect_skills_never_join_the_bundled_pm_roster() {
    // Drift guard: a bundled skill deploys to every PM's managed tier.
    let bundled = crate::core::skill_retire::bundled_skill_stems(
        crate::core::content_source::test_support::rc(),
    );
    assert!(!bundled.is_empty(), "the bundled roster read as empty");
    for skill in SHIPPED {
        assert!(
            !bundled.contains(skill.stem),
            "{} is in the bundled PM skill roster",
            skill.stem
        );
    }
}
